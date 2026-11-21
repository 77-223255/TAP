//! X11 + tiny-skia 界面。
//!
//! 不用任何 GUI 框架：在 CLAP 宿主给的父窗口下建一个子窗口，CPU 画进
//! tiny-skia 的 Pixmap，再（优先走 MIT-SHM）推给 X11。没有 GL 依赖。
//!
//! 视觉：石墨灰的实体感面板 —— 一点点渐变、1px 高光边、内凹的槽，
//! 一个青绿色强调色，没有花活。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tiny_skia::{
    Color, FillRule, LinearGradient, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode,
    Stroke, Transform,
};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::ConnectionExt as ShmExt;
use x11rb::protocol::xproto::{
    ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_FROM_PARENT;

use crate::capture::Capture;
use crate::ctl::{zoom_index, Ctl};

/// 100% 缩放下的逻辑尺寸，所有布局都按这个坐标系写
pub const BASE_W: f32 = 600.0;
pub const BASE_H: f32 = 372.0;

const HEADER_H: f32 = 62.0;
const BODY_Y: f32 = 74.0;
const FOOTER_H: f32 = 54.0;
const PAD: f32 = 18.0;
const LEFT_W: f32 = 286.0;
const RIGHT_X: f32 = PAD + LEFT_W + 12.0;
const RIGHT_W: f32 = BASE_W - PAD - RIGHT_X;
const BODY_H: f32 = BASE_H - FOOTER_H - 12.0 - BODY_Y;
const KNOB_RANGE: f32 = 24.0; // ±24 dB

pub fn size_for(scale: f32) -> (u32, u32) {
    (
        (BASE_W * scale).round().max(1.0) as u32,
        (BASE_H * scale).round().max(1.0) as u32,
    )
}

// ------------------------------------------------------------------ 配色
fn bg_top() -> Color {
    Color::from_rgba8(0x1a, 0x1e, 0x23, 255)
}
fn bg_bot() -> Color {
    Color::from_rgba8(0x0c, 0x0e, 0x11, 255)
}
fn panel_top() -> Color {
    Color::from_rgba8(0x24, 0x28, 0x2e, 255)
}
fn panel_bot() -> Color {
    Color::from_rgba8(0x18, 0x1c, 0x21, 255)
}
fn plate_top() -> Color {
    Color::from_rgba8(0x2a, 0x2f, 0x36, 255)
}
fn edge() -> Color {
    Color::from_rgba8(0x05, 0x06, 0x08, 255)
}
fn well() -> Color {
    Color::from_rgba8(0x08, 0x09, 0x0b, 255)
}
fn text() -> Color {
    Color::from_rgba8(0xe9, 0xed, 0xf1, 255)
}
fn dim() -> Color {
    Color::from_rgba8(0x8b, 0x95, 0x9f, 255)
}
fn dim2() -> Color {
    Color::from_rgba8(0x5c, 0x65, 0x6e, 255)
}
fn accent() -> Color {
    Color::from_rgba8(0x4f, 0xc3, 0xb6, 255)
}
fn accent_dk() -> Color {
    Color::from_rgba8(0x25, 0x6b, 0x64, 255)
}
fn amber() -> Color {
    Color::from_rgba8(0xd9, 0xa2, 0x5b, 255)
}
fn red() -> Color {
    Color::from_rgba8(0xd0, 0x60, 0x50, 255)
}
fn hi_soft() -> Color {
    Color::from_rgba8(0xff, 0xff, 0xff, 14)
}

// ------------------------------------------------------------------ 字体
struct Fonts {
    regular: fontdue::Font,
  cjk: bool,
}

impl Fonts {
    fn load() -> Option<Self> {
        let settings = fontdue::FontSettings {
            collection_index: 0,
            ..Default::default()
        };
        let cjk_paths = [
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        ];
        if let Some(f) = cjk_paths
            .iter()
            .find_map(|p| fontdue::Font::from_bytes(std::fs::read(p).ok()?, settings).ok())
        {
            return Some(Self { regular: f, cjk: true });
        }
        let latin_paths = [
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/Roboto-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        ];
        let f = latin_paths
            .iter()
            .find_map(|p| fontdue::Font::from_bytes(std::fs::read(p).ok()?, settings).ok())?;
        Some(Self { regular: f, cjk: false })
    }
}

// ------------------------------------------------------------------ 绘制
struct Painter {
    pix: Pixmap,
    fonts: Fonts,
    s: f32,
    cjk: bool,
}

impl Painter {
    fn new(w: u32, h: u32, s: f32) -> Option<Self> {
        let fonts = Fonts::load()?;
        let cjk = fonts.cjk;
        Some(Self {
            pix: Pixmap::new(w, h)?,
            fonts,
            s,
            cjk,
        })
    }

    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        let s = self.s;
        let r = tiny_skia::Rect::from_xywh(x * s, y * s, w * s, h * s);
        if let Some(r) = r {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            self.pix
                .fill_rect(r, &p, Transform::identity(), None);
        }
    }

    fn vgrad(&mut self, x: f32, y: f32, w: f32, h: f32, top: Color, bot: Color) {
        let s = self.s;
        let Some(r) = tiny_skia::Rect::from_xywh(x * s, y * s, w * s, h * s) else {
            return;
        };
        let Some(sh) = LinearGradient::new(
            Point::from_xy(0.0, y * s),
            Point::from_xy(0.0, (y + h) * s),
            vec![
                tiny_skia::GradientStop::new(0.0, top),
                tiny_skia::GradientStop::new(1.0, bot),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let mut p = Paint::default();
        p.shader = sh;
        p.anti_alias = true;
        self.pix.fill_rect(r, &p, Transform::identity(), None);
    }

    fn round_path(x: f32, y: f32, w: f32, h: f32, r: f32, s: f32) -> Option<tiny_skia::Path> {
        let (x, y, w, h) = (x * s, y * s, w * s, h * s);
        let r = (r * s).min(w / 2.0).min(h / 2.0);
        let mut pb = PathBuilder::new();
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.quad_to(x + w, y, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.quad_to(x + w, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.quad_to(x, y + h, x, y + h - r);
        pb.line_to(x, y + r);
        pb.quad_to(x, y, x + r, y);
        pb.close();
        pb.finish()
    }

    fn round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        let Some(path) = Self::round_path(x, y, w, h, r, self.s) else {
            return;
        };
        let mut p = Paint::default();
        p.set_color(color);
        p.anti_alias = true;
        self.pix
            .fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }

    fn round_vgrad(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, top: Color, bot: Color) {
        let s = self.s;
        let Some(path) = Self::round_path(x, y, w, h, r, s) else {
            return;
        };
        let Some(sh) = LinearGradient::new(
            Point::from_xy(0.0, y * s),
            Point::from_xy(0.0, (y + h) * s),
            vec![
                tiny_skia::GradientStop::new(0.0, top),
                tiny_skia::GradientStop::new(1.0, bot),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let mut p = Paint::default();
        p.shader = sh;
        p.anti_alias = true;
        self.pix
            .fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }

    fn stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color, width: f32) {
        let s = self.s;
        let Some(path) = Self::round_path(x, y, w, h, r, s) else {
            return;
        };
        let mut p = Paint::default();
        p.set_color(color);
        p.anti_alias = true;
        let stroke = Stroke {
            width: width * s,
            ..Default::default()
        };
        self.pix
            .stroke_path(&path, &p, &stroke, Transform::identity(), None);
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
                .fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }

    fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, color: Color, width: f32) {
        let s = self.s;
        let steps = 48;
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
            let stroke = Stroke {
                width: width * s,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            self.pix
                .stroke_path(&path, &p, &stroke, Transform::identity(), None);
        }
    }

    /// 面板：内凹槽 + 顶部 1px 高光 + 底部 1px 暗边，模拟实体压出来的板子
    fn panel(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        self.round_vgrad(x, y, w, h, r, panel_top(), panel_bot());
        self.stroke(x, y, w, h, r, edge(), 1.0);
        self.fill(x + 1.0, y + 1.0, w - 2.0, 1.0, hi_soft());
        self.fill(x + 1.0, y + h - 2.0, w - 2.0, 1.0, Color::from_rgba8(0, 0, 0, 90));
    }

    /// 槽：比面板更暗，顶部一道内阴影
    fn slot(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        self.round(x, y, w, h, r, well());
        self.stroke(x, y, w, h, r, Color::from_rgba8(0, 0, 0, 200), 1.0);
        self.fill(x + 2.0, y + 1.0, w - 4.0, 1.0, Color::from_rgba8(0, 0, 0, 160));
        self.fill(x + 2.0, y + h - 2.0, w - 4.0, 1.0, Color::from_rgba8(255, 255, 255, 8));
    }

    fn text(&mut self, x: f32, baseline: f32, size: f32, spacing: f32, s: &str, color: Color) {
        self.text_w(x, baseline, size, spacing, s, color, 0.0);
    }

    fn text_b(&mut self, x: f32, baseline: f32, size: f32, spacing: f32, s: &str, color: Color) {
        self.text_w(x, baseline, size, spacing, s, color, 0.55);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_w(
        &mut self,
        x: f32,
        baseline: f32,
        size: f32,
        spacing: f32,
        s: &str,
        color: Color,
        weight: f32,
    ) {
        let sc = self.s;
        let px = size * sc;
        let x0 = x * sc;
        let base = baseline * sc;
        let Self { pix, fonts, .. } = self;
        let pw = pix.width() as i32;
        let ph = pix.height() as i32;
        let data = pix.data_mut();
        let cr = (color.red() * 255.0) as u8;
        let cg = (color.green() * 255.0) as u8;
        let cb = (color.blue() * 255.0) as u8;
        let passes = if weight > 0.0 { 2 } else { 1 };

        for pass in 0..passes {
            let dx = if pass == 0 { 0.0 } else { weight * 0.6 * sc };
            let mut pen = x0;
            for ch in s.chars() {
                if ch == ' ' {
                    pen += fonts.regular.metrics(ch, px).advance_width + spacing * sc;
                    continue;
                }
                let (m, bitmap) = fonts.regular.rasterize(ch, px);
                let gx = (pen + dx).round() as i32 + m.xmin;
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
    }

    fn text_w_of(&self, s: &str, size: f32, spacing: f32) -> f32 {
        let px = size * self.s;
        let mut w = 0.0;
        for ch in s.chars() {
            w += self.fonts.regular.metrics(ch, px).advance_width + spacing * self.s;
        }
        w / self.s
    }

    fn text_right(&mut self, right: f32, baseline: f32, size: f32, spacing: f32, s: &str, c: Color) {
        let w = self.text_w_of(s, size, spacing);
        self.text(right - w, baseline, size, spacing, s, c);
    }

}

// ------------------------------------------------------------------ 视图数据
pub struct View {
    pub sinks: Vec<String>,
    pub source: i32,
    pub running: bool,
    pub gain_db: f32,
    pub meters: [f32; 2],
    pub holds: [f32; 2],
    pub buffer_secs: f32,
    pub host_rate: u32,
    pub card_rate: u32,
    pub zoom: i32,
    pub error: Option<String>,
    pub dropped: u64,
}

impl View {
    pub fn from_state(ctl: &Ctl, cap: &Capture) -> Self {
        let sinks = cap.sinks().into_iter().map(|s| s.label).collect();
        let err = cap.error();
        Self {
            sinks,
            source: ctl.source(),
            running: cap.is_running(),
            gain_db: ctl.gain_db(),
            meters: [ctl.meter(0), ctl.meter(1)],
            holds: [ctl.hold(0), ctl.hold(1)],
            buffer_secs: ctl.fill() as f32 / ctl.sr().max(1) as f32,
            host_rate: ctl.sr(),
            card_rate: cap.rate(),
            zoom: ctl.zoom(),
            error: if err.is_empty() { None } else { Some(err) },
            dropped: cap.dropped_frames(),
        }
    }
}

// ------------------------------------------------------------------ 布局
#[derive(Default)]
pub struct Layout {
    rows: Vec<(f32, f32, f32, f32, usize)>,
    zoom_btns: Vec<(f32, f32, f32, f32, usize)>,
    toggle: (f32, f32, f32, f32),
    knob: (f32, f32, f32),
    scroll: usize,
    drag: Option<(f32, f32)>,
}

fn inside(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
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

    /// 宿主自己改了窗口尺寸：换算成最接近的档位
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

// shm 指针只在 GUI 线程用
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
        // 标记删除：最后 detach 时内核回收
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

fn run_ui(ctl: Arc<Ctl>, cap: Arc<Capture>, quit: Arc<AtomicBool>, host: usize, parent: u32) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = conn.setup().roots[screen_num].clone();
    let has_shm = conn
        .extension_information(x11rb::protocol::shm::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();

    let depth = screen.root_depth;
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
    let mut layout = Layout {
        scroll: 0,
        ..Default::default()
    };
    let mut last = Instant::now() - Duration::from_secs(1);

    while !quit.load(Ordering::Relaxed) {
        // 缩放在任何地方被改（界面点按、宿主、参数自动化）都在这里落地
        let want = ctl.zoom_scale();
        if (want - scale).abs() > 1e-4 {
            scale = want;
            let (nw, nh) = size_for(scale);
            // 先重建画面缓冲，成功再改窗口，避免闪一下空白
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
                    if b.detail == 4 || b.detail == 5 {
                        let dir = if b.detail == 4 { -1i32 } else { 1 };
                        layout.scroll = (layout.scroll as i32 + dir).max(0) as usize;
                        continue;
                    }
                    if b.detail == 3 {
                        if inside(
                            (layout.knob.0 - layout.knob.2, layout.knob.1 - layout.knob.2, layout.knob.2 * 2.0, layout.knob.2 * 2.0),
                            x,
                            y,
                        ) {
                            ctl.set_gain_db(0.0);
                        }
                        continue;
                    }
                    if b.detail != 1 {
                        continue;
                    }
                    if inside(layout.toggle, x, y) {
                        if cap.is_running() {
                            cap.stop();
                        } else {
                            start_selected(&ctl, &cap);
                        }
                    } else if inside(
                        (layout.knob.0 - layout.knob.2, layout.knob.1 - layout.knob.2, layout.knob.2 * 2.0, layout.knob.2 * 2.0),
                        x,
                        y,
                    ) {
                        layout.drag = Some((y, ctl.gain_db()));
                    } else if let Some(b) = layout.zoom_btns.iter().find(|b| inside((b.0, b.1, b.2, b.3), x, y)) {
                        ctl.set_zoom(b.4 as i32);
                        // 主线程收到回调后向宿主申请新尺寸
                        request_host_callback(host);
                    } else {
                        let hit = layout
                            .rows
                            .iter()
                            .find(|r| inside((r.0, r.1, r.2, r.3), x, y))
                            .cloned();
                        if let Some(row) = hit {
                            if row.4 as i32 != ctl.source() {
                                ctl.set_source(row.4 as i32);
                                if cap.is_running() {
                                    start_selected(&ctl, &cap);
                                }
                            }
                        }
                    }
                }
                x11rb::protocol::Event::MotionNotify(m) => {
                    if let Some((y0, db0)) = layout.drag {
                        let y = m.event_y as f32 / scale;
                        let db = (db0 - (y - y0) * (KNOB_RANGE / 90.0)).clamp(-KNOB_RANGE, KNOB_RANGE);
                        let db = if db.abs() < 0.5 { 0.0 } else { db };
                        ctl.set_gain_db(db);
                    }
                }
                x11rb::protocol::Event::ButtonRelease(_) => layout.drag = None,
                x11rb::protocol::Event::DestroyNotify(_) => return,
                x11rb::protocol::Event::Error(e) => {
                    // 多半是 SHM 在这个 X server / 深度上不好用：退回普通 PutImage
                    if shm.take().is_some() {
                        eprintln!("tap: X 错误 {e:?}，SHM 上屏关闭，改用 PutImage");
                        last = Instant::now() - Duration::from_secs(1);
                    }
                }
                x11rb::protocol::Event::ConfigureNotify(c) => {
                    // 宿主给的空间变了：按它的尺寸重新换算档位
                    if c.width as u32 != w || c.height as u32 != h {
                        let idx = zoom_index(c.width as f32 / BASE_W);
                        ctl.set_zoom_quiet(idx);
                    }
                }
                _ => {}
            }
        }

        // 捕获中电平一直在动，20fps 够用；放大后像素多，降一点帧率
        let fps = if scale >= 2.0 { 12 } else { 20 };
        if last.elapsed() >= Duration::from_millis(1000 / fps) {
            let v = View::from_state(&ctl, &cap);
            paint(&mut painter, &v, &mut layout);
            match shm.as_mut() {
                Some(s) => push_shm(&conn, win, gc, &painter.pix, w, h, s, depth),
                None => push_image(&conn, win, gc, &painter.pix, w, h, depth),
            }
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(6));
    }

    let _ = conn.destroy_window(win);
    let _ = conn.free_gc(gc);
    let _ = conn.flush();
}

/// index 0 = 跟随系统默认输出
fn start_selected(ctl: &Arc<Ctl>, cap: &Arc<Capture>) {
    let idx = ctl.source().max(0);
    let sink = if idx == 0 {
        None
    } else {
        cap.sinks()
            .get((idx - 1) as usize)
            .map(|s| s.name.clone())
    };
    cap.start(sink.as_deref());
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
            // tiny-skia 是预乘 alpha，X11 要非预乘
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
        win,
        gc,
        w as u16,
        h as u16,
        0,
        0,
        w as u16,
        h as u16,
        0,
        0,
        depth,
        2, // ZPixmap
        false,
        shm.seg,
        0,
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
const ZOOM_LABELS: [&str; 6] = ["50", "100", "150", "200", "250", "300"];

fn paint(p: &mut Painter, v: &View, l: &mut Layout) {
    p.vgrad(0.0, 0.0, BASE_W, BASE_H, bg_top(), bg_bot());

    // ───────── 顶栏
    p.vgrad(0.0, 0.0, BASE_W, HEADER_H, plate_top(), panel_bot());
    p.fill(0.0, HEADER_H - 1.0, BASE_W, 1.0, edge());
    p.fill(0.0, 0.0, BASE_W, 1.0, Color::from_rgba8(255, 255, 255, 18));

    p.text_b(PAD, 36.0, 25.0, 3.4, "TAP", text());
    p.text(PAD, 52.0, 8.0, 1.9, "PIPEWIRE · SYSTEM AUDIO CAPTURE", dim2());

    // 状态
    let (ok_running, status, status_col) = if v.error.is_some() {
        (false, "ERROR", amber())
    } else if v.running {
        (true, "LIVE", accent())
    } else {
        (false, "IDLE", dim2())
    };
    p.text_right(BASE_W - PAD, 26.0, 9.0, 1.6, status, status_col);
    let sw = p.text_w_of(status, 9.0, 1.6);
    let dot_x = BASE_W - PAD - sw - 9.0;
    if ok_running {
        p.circle(dot_x, 22.5, 5.0, Color::from_rgba8(0x4f, 0xc3, 0xb6, 48));
    }
    p.circle(dot_x, 22.5, 2.6, status_col);

    // 缩放档位
    let cell_w = 30.0f32;
    let gap = 3.0f32;
    let total = cell_w * 6.0 + gap * 5.0;
    let zx = BASE_W - PAD - total;
    let zy = 34.0;
    p.text_right(zx - 8.0, 47.0, 7.5, 1.4, "ZOOM", dim2());
    l.zoom_btns.clear();
    for i in 0..6 {
        let x = zx + i as f32 * (cell_w + gap);
        let sel = v.zoom as usize == i;
        if sel {
            p.round_vgrad(x, zy, cell_w, 17.0, 3.0, Color::from_rgba8(0x31, 0x6f, 0x69, 255), accent_dk());
            p.stroke(x, zy, cell_w, 17.0, 3.0, edge(), 1.0);
        } else {
            p.round(x, zy, cell_w, 17.0, 3.0, Color::from_rgba8(0x1b, 0x1f, 0x24, 255));
            p.stroke(x, zy, cell_w, 17.0, 3.0, Color::from_rgba8(0, 0, 0, 140), 1.0);
        }
        let lab = ZOOM_LABELS[i];
        let tw = p.text_w_of(lab, 8.0, 0.6);
        p.text(
            x + (cell_w - tw) / 2.0,
            zy + 11.5,
            8.0,
            0.6,
            lab,
            if sel { text() } else { dim2() },
        );
        l.zoom_btns.push((x, zy, cell_w, 17.0, i));
    }

    // ───────── 输入源
    p.panel(PAD, BODY_Y, LEFT_W, BODY_H, 6.0);
    p.text(PAD + 14.0, BODY_Y + 21.0, 8.0, 1.9, "SOURCE", dim2());

    let row_h = 28.0;
    let top = BODY_Y + 30.0;
    let max_rows = ((BODY_H - 40.0) / row_h).max(1.0) as usize;
    let count = v.sinks.len() + 1;
    if l.scroll + max_rows > count {
        l.scroll = count.saturating_sub(max_rows);
    }

    let sel = v.source.max(0) as usize;
    l.rows.clear();
    if v.sinks.is_empty() {
        p.text(PAD + 16.0, top + 18.0, 10.5, 0.2, "waiting for pipewire…", dim2());
    }
    for i in l.scroll..count.min(l.scroll + max_rows) {
        let y = top + ((i - l.scroll) as f32) * row_h;
        let focused = i == sel;
        let label = if i == 0 {
            if p.cjk { "系统默认输出".to_string() } else { "System default".to_string() }
        } else {
            v.sinks[i - 1].clone()
        };
        if focused {
            p.round(PAD + 8.0, y, LEFT_W - 16.0, row_h - 4.0, 4.0, Color::from_rgba8(0x2b, 0x31, 0x38, 255));
            p.fill(PAD + 8.0, y + 5.0, 2.0, row_h - 14.0, accent());
        }
        p.circle(PAD + 20.0, y + 11.0, 3.0, if focused { accent() } else { dim2() });
        let shown = truncate(p, &label, LEFT_W - 56.0, 10.5, 0.2);
        p.text(PAD + 30.0, y + 15.0, 10.5, 0.2, &shown, if focused { text() } else { dim() });
        l.rows.push((PAD + 8.0, y, LEFT_W - 16.0, row_h - 4.0, i));
    }

    // ───────── 右侧：增益旋钮 + 电平
    p.panel(RIGHT_X, BODY_Y, RIGHT_W, BODY_H, 6.0);
    let cx = RIGHT_X + 62.0;
    let cy = BODY_Y + 104.0;
    let kr = 38.0;
    l.knob = (cx, cy, kr);

    // 旋钮外圈槽
    p.circle(cx, cy, kr + 7.0, Color::from_rgba8(0x0b, 0x0d, 0x0f, 255));
    p.arc(cx, cy, kr + 4.0, -2.356, 2.356, Color::from_rgba8(0x33, 0x39, 0x40, 255), 2.0);
    let t = ((v.gain_db + KNOB_RANGE) / (KNOB_RANGE * 2.0)).clamp(0.0, 1.0);
    p.arc(cx, cy, kr + 4.0, -2.356, -2.356 + 4.712 * t, accent(), 2.0);

    // 刻度
    for k in 0..9 {
        let a = -2.356 + 4.712 * (k as f32 / 8.0);
        let r0 = kr + 9.0;
        let r1 = if k == 4 { kr + 13.0 } else { kr + 11.0 };
        let (x0, y0) = (cx + a.sin() * r0, cy - a.cos() * r0);
        let (x1, y1) = (cx + a.sin() * r1, cy - a.cos() * r1);
        let s = p.s;
        let mut pb = PathBuilder::new();
        pb.move_to(x0 * s, y0 * s);
        pb.line_to(x1 * s, y1 * s);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(if k == 4 { dim() } else { Color::from_rgba8(0x3d, 0x45, 0x4d, 255) });
            paint.anti_alias = true;
            let st = Stroke {
                width: 1.0 * s,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            p.pix.stroke_path(&path, &paint, &st, Transform::identity(), None);
        }
    }

    // 旋钮本体：径向渐变，左上亮
    {
        let s = p.s;
        let mut pb = PathBuilder::new();
        pb.push_circle(cx * s, cy * s, kr * s);
        if let Some(path) = pb.finish() {
            if let Some(sh) = RadialGradient::new(
                Point::from_xy((cx - kr * 0.35) * s, (cy - kr * 0.45) * s),
                0.0,
                Point::from_xy(cx * s, cy * s),
                kr * 1.35 * s,
                vec![
                    tiny_skia::GradientStop::new(0.0, Color::from_rgba8(0x4a, 0x52, 0x5b, 255)),
                    tiny_skia::GradientStop::new(0.55, Color::from_rgba8(0x2c, 0x32, 0x39, 255)),
                    tiny_skia::GradientStop::new(1.0, Color::from_rgba8(0x14, 0x17, 0x1b, 255)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut paint = Paint::default();
                paint.shader = sh;
                paint.anti_alias = true;
                p.pix
                    .fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
    p.arc(cx, cy, kr - 0.5, -3.1416, 3.1416, Color::from_rgba8(0, 0, 0, 110), 1.0);
    // 指针
    {
        let ang = -2.356 + 4.712 * t;
        let (sx, sy) = (cx + ang.sin() * kr * 0.30, cy - ang.cos() * kr * 0.30);
        let (ex, ey) = (cx + ang.sin() * kr * 0.86, cy - ang.cos() * kr * 0.86);
        let s = p.s;
        let mut pb = PathBuilder::new();
        pb.move_to(sx * s, sy * s);
        pb.line_to(ex * s, ey * s);
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(text());
            paint.anti_alias = true;
            let st = Stroke {
                width: 2.2 * s,
                line_cap: tiny_skia::LineCap::Round,
                ..Default::default()
            };
            p.pix.stroke_path(&path, &paint, &st, Transform::identity(), None);
        }
    }
    p.circle(cx, cy, kr * 0.16, Color::from_rgba8(0x39, 0x40, 0x48, 255));

    let gw = p.text_w_of("GAIN", 8.0, 1.9);
    p.text(cx - gw / 2.0, BODY_Y + 184.0, 8.0, 1.9, "GAIN", dim2());
    let gtxt = if v.gain_db.abs() < 0.05 {
        "0.0 dB".to_string()
    } else {
        format!("{:+.1} dB", v.gain_db)
    };
    p.text(cx - p.text_w_of(&gtxt, 12.0, 0.3) / 2.0, BODY_Y + 204.0, 12.0, 0.3, &gtxt, text());

    // 电平表
    let bar_w = 42.0;
    let bar_gap = 12.0;
    let mx = RIGHT_X + RIGHT_W - 34.0 - bar_w * 2.0 - bar_gap;
    let mt = BODY_Y + 40.0;
    let mh = BODY_H - 76.0;
    p.text(mx, mt - 8.0, 7.5, 1.4, "OUTPUT", dim2());
    for ch in 0..2 {
        let x = mx + ch as f32 * (bar_w + bar_gap);
        p.slot(x, mt, bar_w, mh, 3.0);
        let level = db_norm(v.meters[ch]);
        if level > 0.004 {
            let fh = (mh * level).max(2.0);
            let s = p.s;
            if let Some(path) = Painter::round_path(x + 2.0, mt + mh - fh, bar_w - 4.0, fh, 2.0, s) {
                if let Some(sh) = LinearGradient::new(
                    Point::from_xy(0.0, (mt + mh) * s),
                    Point::from_xy(0.0, mt * s),
                    vec![
                        tiny_skia::GradientStop::new(0.0, Color::from_rgba8(0x27, 0x66, 0x60, 255)),
                        tiny_skia::GradientStop::new(0.74, accent()),
                        tiny_skia::GradientStop::new(0.87, accent()),
                        tiny_skia::GradientStop::new(0.95, amber()),
                        tiny_skia::GradientStop::new(1.0, red()),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let mut paint = Paint::default();
                    paint.shader = sh;
                    paint.anti_alias = true;
                    p.pix
                        .fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        let hold = db_norm(v.holds[ch]);
        if hold > 0.004 {
            let hy = mt + mh * (1.0 - hold);
            p.fill(x + 2.0, hy - 1.0, bar_w - 4.0, 1.5, Color::from_rgba8(255, 255, 255, 150));
        }
        let db = norm_db(level);
        let txt = if db < -79.0 { "-∞".to_string() } else { format!("{db:.0}") };
        p.text_right(x + bar_w, mt + mh + 13.0, 8.5, 0.4, &txt, dim());
    }
    // dB 刻度
    for db in [-6.0f32, -12.0, -24.0, -36.0, -48.0] {
        let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let y = mt + mh * (1.0 - t);
        p.fill(mx - 4.0, y, 3.0, 1.0, Color::from_rgba8(0x3b, 0x42, 0x4a, 255));
        p.text_right(BASE_W - PAD - 4.0, y + 3.0, 7.0, 0.2, &format!("{db:.0}"), dim2());
    }

    // ───────── 底栏
    let by = BASE_H - FOOTER_H + 8.0;
    let bh = 38.0;
    l.toggle = (PAD, by, 148.0, bh);
    if v.running {
        p.round_vgrad(PAD, by, 148.0, bh, 5.0, Color::from_rgba8(0xc0, 0x5a, 0x4b, 255), Color::from_rgba8(0x8d, 0x3c, 0x33, 255));
        p.stroke(PAD, by, 148.0, bh, 5.0, edge(), 1.0);
        p.fill(PAD + 1.0, by + 1.0, 146.0, 1.0, Color::from_rgba8(255, 255, 255, 40));
        p.circle(PAD + 22.0, by + bh / 2.0, 4.0, Color::from_rgba8(0x2a, 0x10, 0x0c, 200));
        p.text(PAD + 36.0, by + 24.0, 12.5, 1.4, "STOP", Color::from_rgba8(0xfb, 0xef, 0xec, 255));
    } else {
        p.round_vgrad(PAD, by, 148.0, bh, 5.0, Color::from_rgba8(0x3d, 0x9c, 0x91, 255), Color::from_rgba8(0x24, 0x68, 0x61, 255));
        p.stroke(PAD, by, 148.0, bh, 5.0, edge(), 1.0);
        p.fill(PAD + 1.0, by + 1.0, 146.0, 1.0, Color::from_rgba8(255, 255, 255, 52));
        p.circle(PAD + 22.0, by + bh / 2.0, 4.0, Color::from_rgba8(0x0a, 0x24, 0x21, 200));
        p.text(PAD + 36.0, by + 24.0, 12.5, 1.4, "CAPTURE", Color::from_rgba8(0x08, 0x1c, 0x1a, 255));
    }

    // 采样率 / 缓冲
    let host = fmt_rate(v.host_rate);
    let card = if v.card_rate == 0 { "—".to_string() } else { fmt_rate(v.card_rate) };
    let line1 = if v.card_rate == 0 || v.card_rate == v.host_rate {
        format!("HOST {host} · CARD {card}")
    } else {
        format!("HOST {host} · CARD {card} · RESAMPLED")
    };
    let ms = v.buffer_secs * 1000.0;
    let line2 = format!("BUFFER {ms:.0} ms · DROP {}", v.dropped);

    p.text_right(
        BASE_W - PAD,
        by + 16.0,
        8.5,
        1.1,
        &line1,
        if v.card_rate != 0 && v.card_rate != v.host_rate { amber() } else { dim2() },
    );
    match &v.error {
        Some(err) => {
            let shown = truncate(p, err, BASE_W - PAD * 2.0, 8.5, 0.6);
            p.text_right(BASE_W - PAD, by + 30.0, 8.5, 0.6, &shown, amber());
        }
        None => p.text_right(BASE_W - PAD, by + 30.0, 8.5, 1.1, &line2, dim2()),
    }
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

fn fmt_rate(r: u32) -> String {
    if r == 0 {
        return "—".to_string();
    }
    format!("{:.1} kHz", r as f32 / 1000.0)
}

fn truncate(p: &Painter, s: &str, max_w: f32, size: f32, spacing: f32) -> String {
    if p.text_w_of(s, size, spacing) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let mut test = out.clone();
        test.push(ch);
        test.push('…');
        if p.text_w_of(&test, size, spacing) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// 线性电平 → 0..1（-60 dB .. 0 dB）
fn db_norm(v: f32) -> f32 {
    if v <= 0.000_001 {
        return 0.0;
    }
    let db = 20.0 * v.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn norm_db(level: f32) -> f32 {
    if level <= 0.0 {
        -80.0
    } else {
        level * 60.0 - 60.0
    }
}
