//! X11 + tiny-skia 的界面。
//!
//! 不用任何 GUI 框架：直接在 CLAP 宿主给的父窗口下面建一个子窗口，
//! CPU 画进 tiny-skia 的 Pixmap，再 XPutImage 上去。没有 GL 依赖，
//! 在 Hyprland / XWayland 下都一样。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tiny_skia::{
    Color, FillRule, LinearGradient, Paint, PathBuilder, Pixmap, SpreadMode, Stroke, Transform,
};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_FROM_PARENT;

use crate::ctl::Ctl;

pub const WIDTH: u32 = 588;
pub const HEIGHT: u32 = 372;

// ------------------------------------------------------------------ 配色
fn bg() -> Color {
    Color::from_rgba8(0x15, 0x12, 0x10, 255)
}
fn panel() -> Color {
    Color::from_rgba8(0x1d, 0x19, 0x16, 255)
}
fn panel_hi() -> Color {
    Color::from_rgba8(0x2d, 0x26, 0x20, 255)
}
fn line() -> Color {
    Color::from_rgba8(0x32, 0x2b, 0x25, 255)
}
fn text() -> Color {
    Color::from_rgba8(0xec, 0xe5, 0xdc, 255)
}
fn dim() -> Color {
    Color::from_rgba8(0x96, 0x8a, 0x7e, 255)
}
fn accent() -> Color {
    Color::from_rgba8(0xd9, 0x77, 0x57, 255)
}
fn ok() -> Color {
    Color::from_rgba8(0x8b, 0xb0, 0x73, 255)
}
fn warn() -> Color {
    Color::from_rgba8(0xe0, 0xb4, 0x5c, 255)
}
fn well() -> Color {
    Color::from_rgba8(0x12, 0x0f, 0x0d, 255)
}

// ------------------------------------------------------------------ 字体
struct Fonts {
    regular: fontdue::Font,
    cjk: bool,
}

impl Fonts {
    fn load() -> Option<Self> {
        let read = |p: &str| std::fs::read(p).ok();
        let settings = fontdue::FontSettings {
            collection_index: 0,
            ..Default::default()
        };

        let mut cjk = false;
        let regular = ["/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"]
            .iter()
            .filter_map(|p| read(p))
            .find_map(|d| fontdue::Font::from_bytes(d, settings).ok())
            .map(|f| {
                cjk = true;
                f
            })
            .or_else(|| {
                [
                    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                    "/usr/share/fonts/TTF/Roboto-Regular.ttf",
                ]
                .iter()
                .filter_map(|p| read(p))
                .find_map(|d| fontdue::Font::from_bytes(d, settings).ok())
            })?;

        Some(Self { regular, cjk })
    }
}

/// 中英双语标签：有 CJK 字体就用中文，否则退回英文。
struct L {
    cjk: bool,
}

impl L {
    fn title(&self) -> &'static str {
        if self.cjk {
            "内录"
        } else {
            "NEILU"
        }
    }
    fn subtitle(&self) -> &'static str {
        "PIPEWIRE CAPTURE"
    }
    fn inputs(&self) -> &'static str {
        if self.cjk {
            "输入源"
        } else {
            "INPUT SOURCE"
        }
    }
    fn gain(&self) -> &'static str {
        if self.cjk {
            "增益"
        } else {
            "GAIN"
        }
    }
    fn buffer(&self) -> &'static str {
        if self.cjk {
            "缓冲"
        } else {
            "BUFFER"
        }
    }
    fn start(&self) -> &'static str {
        if self.cjk {
            "开始录制"
        } else {
            "START"
        }
    }
    fn stop(&self) -> &'static str {
        if self.cjk {
            "停止"
        } else {
            "STOP"
        }
    }
    fn live(&self) -> &'static str {
        if self.cjk {
            "录制中"
        } else {
            "LIVE"
        }
    }
    fn idle(&self) -> &'static str {
        if self.cjk {
            "待机"
        } else {
            "IDLE"
        }
    }
    fn auto_label(&self) -> &'static str {
        if self.cjk {
            "跟随系统默认输出"
        } else {
            "System default"
        }
    }
    fn no_device(&self) -> &'static str {
        if self.cjk {
            "没有找到音频输出设备"
        } else {
            "no audio devices found"
        }
    }
    fn hint(&self) -> &'static str {
        if self.cjk {
            "录音模式 → Record: output (stereo)\n关掉输入监听，按下开始，轨道就会自己长出来"
        } else {
            "rec mode → Record: output (stereo)\nkeep input monitoring off, hit START"
        }
    }
}

// ------------------------------------------------------------------ 绘制
struct Painter {
    pix: Pixmap,
    fonts: Fonts,
    lang: L,
}

impl Painter {
    fn new(w: u32, h: u32) -> Option<Self> {
        Some(Self {
            pix: Pixmap::new(w, h)?,
            fonts: Fonts::load()?,
            lang: L { cjk: false },
        })
    }

    fn round_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let r = r.min(w / 2.0).min(h / 2.0);
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
    }

    fn fill_round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        let mut pb = PathBuilder::new();
        Self::round_rect(&mut pb, x, y, w, h, r);
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            self.pix
                .fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }

    fn stroke_round(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color, width: f32) {
        let mut pb = PathBuilder::new();
        Self::round_rect(&mut pb, x, y, w, h, r);
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            let stroke = Stroke {
                width,
                ..Default::default()
            };
            self.pix
                .stroke_path(&path, &p, &stroke, Transform::identity(), None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            self.pix
                .fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }

    fn text_width(&self, s: &str, size: f32) -> f32 {
        s.chars()
            .map(|c| self.fonts.regular.metrics(c, size).advance_width)
            .sum()
    }

    fn text(&mut self, x: f32, baseline: f32, size: f32, s: &str, color: Color) {
        let mut pen = x;
        let w = self.pix.width() as i32;
        let h = self.pix.height() as i32;
        let data = self.pix.data_mut();
        let cr = (color.red() * 255.0) as u8;
        let cg = (color.green() * 255.0) as u8;
        let cb = (color.blue() * 255.0) as u8;

        for ch in s.chars() {
            let (m, bitmap) = self.fonts.regular.rasterize(ch, size);
            let gx = pen.round() as i32 + m.xmin;
            let gy = baseline.round() as i32 - (m.ymin + m.height as i32);
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = bitmap[row * m.width + col];
                    if a == 0 {
                        continue;
                    }
                    let px = gx + col as i32;
                    let py = gy + row as i32;
                    if px < 0 || py < 0 || px >= w || py >= h {
                        continue;
                    }
                    let idx = ((py * w + px) * 4) as usize;
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
            pen += m.advance_width;
        }
    }

    fn text_right(&mut self, right: f32, baseline: f32, size: f32, s: &str, color: Color) {
        let w = self.text_width(s, size);
        self.text(right - w, baseline, size, s, color);
    }
}

// ------------------------------------------------------------------ 布局
#[derive(Default)]
struct Layout {
    rows: Vec<(f32, f32, f32, f32, usize)>,
    toggle: (f32, f32, f32, f32),
    slider: (f32, f32, f32, f32),
    refresh: (f32, f32, f32, f32),
    scroll: usize,
}

fn inside(r: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
}

// ------------------------------------------------------------------ GUI 本体
pub struct Gui {
    ctl: Arc<Ctl>,
    quit: Arc<AtomicBool>,
    size: Arc<Mutex<(u32, u32)>>,
    thread: Option<JoinHandle<()>>,
    attached: bool,
}

impl Gui {
    pub fn new(ctl: Arc<Ctl>) -> Self {
        Self {
            ctl,
            quit: Arc::new(AtomicBool::new(false)),
            size: Arc::new(Mutex::new((WIDTH, HEIGHT))),
            thread: None,
            attached: false,
        }
    }

    pub fn attach(&mut self, parent: u32) -> bool {
        if self.attached {
            return true;
        }
        let ctl = self.ctl.clone();
        let quit = self.quit.clone();
        let size = self.size.clone();
        let handle = thread::Builder::new()
            .name("neilu-gui".into())
            .spawn(move || run_ui(ctl, quit, size, parent));
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
        if let Ok(mut s) = self.size.lock() {
            *s = (w, h);
        }
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

fn run_ui(ctl: Arc<Ctl>, quit: Arc<AtomicBool>, size: Arc<Mutex<(u32, u32)>>, parent: u32) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = conn.setup().roots[screen_num].clone();
    let (mut w, mut h) = size.lock().map(|s| *s).unwrap_or((WIDTH, HEIGHT));

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

    let Some(mut painter) = Painter::new(w, h) else {
        return;
    };
    painter.lang.cjk = painter.fonts.cjk;

    let mut layout = Layout::default();
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut dragging_slider = false;

    while !quit.load(Ordering::Relaxed) {
        // 尺寸变化（已固定大小，但宿主仍可能来消息）
        if let Ok(s) = size.lock() {
            if (s.0, s.1) != (w, h) {
                w = s.0;
                h = s.1;
                painter.pix = match Pixmap::new(w, h) {
                    Some(p) => p,
                    None => break,
                };
                let _ = conn.configure_window(
                    win,
                    &x11rb::protocol::xproto::ConfigureWindowAux::new()
                        .width(w as u32)
                        .height(h as u32),
                );
                last = Instant::now() - Duration::from_secs(1);
            }
        }

        while let Ok(Some(ev)) = conn.poll_for_event() {
            match ev {
                x11rb::protocol::Event::Expose(_) => {
                    last = Instant::now() - Duration::from_secs(1);
                }
                x11rb::protocol::Event::ConfigureNotify(c) => {
                    if (c.width as u32, c.height as u32) != (w, h) {
                        w = c.width as u32;
                        h = c.height as u32;
                        if let Ok(mut s) = size.lock() {
                            *s = (w, h);
                        }
                        painter.pix = match Pixmap::new(w, h) {
                            Some(p) => p,
                            None => return,
                        };
                        last = Instant::now() - Duration::from_secs(1);
                    }
                }
                x11rb::protocol::Event::ButtonPress(b) => {
                    let (x, y) = (b.event_x as f32, b.event_y as f32);
                    if b.detail == 4 || b.detail == 5 {
                        let dir = if b.detail == 4 { -1i32 } else { 1 };
                        layout.scroll = (layout.scroll as i32 + dir).max(0) as usize;
                        continue;
                    }
                    if inside(layout.toggle, x, y) {
                        if ctl.running() {
                            ctl.request_stop();
                        } else {
                            start_selected(&ctl);
                        }
                    } else if inside(layout.slider, x, y) {
                        dragging_slider = true;
                        set_gain_from(&ctl, &layout, x);
                    } else if inside(layout.refresh, x, y) {
                        // 设备列表由 PipeWire 的 registry 事件自动更新，点一下只是刷新内存快照
                        let _ = ctl.sinks();
                    } else {
                        for row in layout.rows.clone() {
                            if inside((row.0, row.1, row.2, row.3), x, y) {
                                ctl.set_source(row.4 as i32);
                                if ctl.running() {
                                    start_selected(&ctl);
                                }
                                break;
                            }
                        }
                    }
                }
                x11rb::protocol::Event::MotionNotify(m) => {
                    if dragging_slider {
                        set_gain_from(&ctl, &layout, m.event_x as f32);
                    }
                }
                x11rb::protocol::Event::ButtonRelease(_) => dragging_slider = false,
                x11rb::protocol::Event::DestroyNotify(_) => return,
                _ => {}
            }
        }

        if last.elapsed() >= Duration::from_millis(33) {
            paint(&mut painter, &ctl, &mut layout, w, h);
            push_image(&conn, win, gc, &painter.pix, w, h);
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(8));
    }

    let _ = conn.destroy_window(win);
    let _ = conn.free_gc(gc);
    let _ = conn.flush();
}

/// 用当前选中的设备开始采集（index 0 = 跟随系统默认输出）
fn start_selected(ctl: &Arc<Ctl>) {
    let idx = ctl.source().max(0);
    let sink = if idx == 0 {
        None
    } else {
        ctl.sinks().get((idx - 1) as usize).map(|s| s.name.clone())
    };
    ctl.request_start(sink);
}

fn set_gain_from(ctl: &Arc<Ctl>, layout: &Layout, x: f32) {
    let (sx, _, sw, _) = layout.slider;
    let t = ((x - sx) / sw).clamp(0.0, 1.0);
    let db = -24.0 + t * 48.0;
    let db = if db.abs() < 0.8 { 0.0 } else { db };
    ctl.set_gain_db(db.round());
}

fn push_image(conn: &RustConnection, win: u32, gc: u32, pix: &Pixmap, w: u32, h: u32) {
    // tiny-skia 给的是预乘 RGBA；X11 的 ZPixmap(24) 要的是 0xRRGGBB
    let src = pix.data();
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for i in 0..(w * h) as usize {
        let r = src[i * 4] as u32;
        let g = src[i * 4 + 1] as u32;
        let b = src[i * 4 + 2] as u32;
        let a = src[i * 4 + 3] as u32;
        let (r, g, b) = if a > 0 && a < 255 {
            (
                (r * 255 / a).min(255),
                (g * 255 / a).min(255),
                (b * 255 / a).min(255),
            )
        } else {
            (r, g, b)
        };
        let v = (r << 16) | (g << 8) | b;
        buf[i * 4] = (v & 0xff) as u8;
        buf[i * 4 + 1] = ((v >> 8) & 0xff) as u8;
        buf[i * 4 + 2] = ((v >> 16) & 0xff) as u8;
        buf[i * 4 + 3] = 0;
    }
    let _ = conn.put_image(
        ImageFormat::Z_PIXMAP,
        win,
        gc,
        w as u16,
        h as u16,
        0,
        0,
        0,
        24,
        &buf,
    );
    let _ = conn.flush();
}

// ------------------------------------------------------------------ 画面
fn paint(p: &mut Painter, ctl: &Arc<Ctl>, layout: &mut Layout, w: u32, h: u32) {
    let wf = w as f32;
    let hf = h as f32;
    let pad = 18.0f32;
    let header_h = 54.0f32;
    let footer_h = 76.0f32;
    let content_y = header_h + 12.0;
    let content_h = (hf - footer_h - content_y).max(120.0);
    let left_w = 292.0f32;
    let right_x = pad + left_w + 16.0;
    let right_w = wf - right_x - pad;

    p.pix.fill(bg());

    // ───────── 顶栏
    p.fill_round(0.0, 0.0, wf, header_h, 0.0, panel());
    p.text(pad, 34.0, 22.0, p.lang.title(), text());
    let title_w = p.text_width(p.lang.title(), 22.0);
    p.text(title_w + pad + 10.0, 33.0, 10.0, p.lang.subtitle(), dim());

    let running = ctl.running();
    let (pill_bg, dot, label) = if running {
        (Color::from_rgba8(0x22, 0x2d, 0x1e, 255), ok(), p.lang.live())
    } else {
        (Color::from_rgba8(0x28, 0x22, 0x1d, 255), dim(), p.lang.idle())
    };
    let pill_w = 96.0f32;
    let px = wf - pill_w - pad;
    p.fill_round(px, 16.0, pill_w, 26.0, 13.0, pill_bg);
    if running {
        p.circle(px + 15.0, 29.0, 8.0, Color::from_rgba8(140, 176, 115, 50));
    }
    p.circle(px + 15.0, 29.0, 4.0, dot);
    p.text(px + 28.0, 34.0, 12.0, label, if running { ok() } else { dim() });

    // ───────── 左侧：输入源
    p.fill_round(pad, content_y, left_w, content_h, 10.0, panel());
    p.stroke_round(pad, content_y, left_w, content_h, 10.0, line(), 1.0);
    p.text(pad + 14.0, content_y + 22.0, 10.0, p.lang.inputs(), dim());

    let sinks = ctl.sinks();
    layout.rows.clear();
    let row_h = 30.0;
    let top = content_y + 34.0;
    let max_rows = ((content_h - 42.0) / row_h).max(1.0) as usize;

    let mut items: Vec<String> = Vec::with_capacity(sinks.len() + 1);
    items.push(p.lang.auto_label().to_string());
    for s in &sinks {
        items.push(s.label.clone());
    }
    if sinks.is_empty() {
        p.text(pad + 14.0, top + 18.0, 12.0, p.lang.no_device(), dim());
    }

    let sel = ctl.source().max(0) as usize;
    for i in layout.scroll..items.len().min(layout.scroll + max_rows) {
        let y = top + ((i - layout.scroll) as f32) * row_h;
        let focused = i == sel;
        if focused {
            p.fill_round(pad + 8.0, y, left_w - 16.0, row_h - 4.0, 7.0, panel_hi());
            p.fill_round(pad + 8.0, y + 4.0, 3.0, row_h - 12.0, 1.5, accent());
        }
        let shown = truncate(&items[i], 30);
        p.text(
            pad + 20.0,
            y + 18.0,
            12.5,
            &shown,
            if focused { text() } else { dim() },
        );
        layout.rows.push((pad + 8.0, y, left_w - 16.0, row_h - 4.0, i));
    }

    // ───────── 右侧：电平 / 缓冲 / 增益
    p.fill_round(right_x, content_y, right_w, content_h, 10.0, panel());
    p.stroke_round(right_x, content_y, right_w, content_h, 10.0, line(), 1.0);
    let rx0 = right_x + 14.0;
    let rw = right_w - 28.0;
    p.text(rx0, content_y + 22.0, 10.0, "OUTPUT · dB", dim());

    let meter_top = content_y + 32.0;
    // 缓冲 / 增益两行贴着面板底部排，剩下的都给电平表，任何高度都不会溢出
    let buf_row_y = content_y + content_h - 48.0;
    let gain_row_y = content_y + content_h - 24.0;
    let meter_h = (buf_row_y - 16.0 - meter_top).max(30.0);
    let gap = 12.0f32;
    let bar_w = ((rw - gap) / 2.0).max(20.0);

    for ch in 0..2 {
        let x = rx0 + ch as f32 * (bar_w + gap);
        p.fill_round(x, meter_top, bar_w, meter_h, 6.0, well());
        let level = db_norm(ctl.meter(ch));
        let hold = db_norm(ctl.hold(ch));
        if level > 0.002 {
            let fh = meter_h * level;
            let mut pb = PathBuilder::new();
            Painter::round_rect(&mut pb, x, meter_top + meter_h - fh, bar_w, fh, 4.0);
            if let Some(path) = pb.finish() {
                let mut paint = Paint::default();
                paint.anti_alias = true;
                if let Some(g) = LinearGradient::new(
                    tiny_skia::Point::from_xy(x, meter_top + meter_h),
                    tiny_skia::Point::from_xy(x, meter_top),
                    vec![
                        tiny_skia::GradientStop::new(0.0, Color::from_rgba8(0x7d, 0xa8, 0x68, 255)),
                        tiny_skia::GradientStop::new(0.62, ok()),
                        tiny_skia::GradientStop::new(0.86, warn()),
                        tiny_skia::GradientStop::new(1.0, accent()),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    paint.shader = g;
                    p.pix
                        .fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        if hold > 0.002 {
            let hy = meter_top + meter_h * (1.0 - hold);
            p.fill_round(
                x + 3.0,
                hy - 1.0,
                (bar_w - 6.0).max(1.0),
                2.0,
                1.0,
                Color::from_rgba8(0xff, 0xf3, 0xe6, 160),
            );
        }
        let db = norm_db(level);
        let txt = if db < -79.0 {
            "-inf".to_string()
        } else {
            format!("{db:.0}")
        };
        let tw = p.text_width(&txt, 10.5);
        p.text(x + (bar_w - tw) / 2.0, meter_top + meter_h + 15.0, 10.5, &txt, dim());
    }

    // 缓冲
    let fill = ctl.fill() as f32;
    let cap = ctl.capacity().max(1) as f32;
    let ratio = (fill / cap).clamp(0.0, 1.0);
    p.text(rx0, buf_row_y + 10.0, 10.0, p.lang.buffer(), dim());
    let bx = rx0 + 44.0;
    let bw = (rw - 44.0 - 62.0).max(20.0);
    p.fill_round(bx, buf_row_y + 2.0, bw, 8.0, 4.0, well());
    if ratio > 0.001 {
        p.fill_round(
            bx,
            buf_row_y + 2.0,
            (bw * ratio).max(2.0),
            8.0,
            4.0,
            Color::from_rgba8(0xd9, 0x77, 0x57, 210),
        );
    }
    let tx = bx + bw * 0.10;
    p.fill_round(tx - 1.0, buf_row_y - 1.0, 2.0, 14.0, 1.0, Color::from_rgba8(0x5a, 0x50, 0x47, 255));
    let ms = fill / (ctl.sr().max(1) as f32) * 1000.0;
    p.text_right(rx0 + rw, buf_row_y + 10.0, 10.5, &format!("{ms:.0} ms"), dim());

    // 增益
    p.text(rx0, gain_row_y + 11.0, 10.0, p.lang.gain(), dim());
    let sx = rx0 + 44.0;
    let sw = bw;
    let sy = gain_row_y + 4.0;
    layout.slider = (sx, sy - 8.0, sw, 22.0);
    p.fill_round(sx, sy, sw, 6.0, 3.0, well());
    p.fill_round(sx, sy, (sw * 0.5).max(1.0), 6.0, 3.0, Color::from_rgba8(0x38, 0x30, 0x29, 255));
    p.fill_round(
        sx + sw * 0.5 - 1.0,
        sy - 3.0,
        2.0,
        12.0,
        1.0,
        Color::from_rgba8(0x5a, 0x50, 0x47, 255),
    );
    let g = ((ctl.gain_db() + 24.0) / 48.0).clamp(0.0, 1.0);
    p.circle(sx + sw * g, sy + 3.0, 7.0, accent());
    let gd = ctl.gain_db();
    let gtxt = if gd.abs() < 0.2 {
        "0.0 dB".to_string()
    } else {
        format!("{gd:+.1} dB")
    };
    p.text_right(rx0 + rw, gain_row_y + 11.0, 10.5, &gtxt, text());

    // ───────── 底栏
    let btn_y = hf - footer_h + 14.0;
    let btn_h = 42.0f32;
    layout.toggle = (pad, btn_y, 168.0, btn_h);
    let (btn_fill, btn_text, label) = if running {
        (accent(), Color::from_rgba8(0x17, 0x11, 0x0e, 255), p.lang.stop())
    } else {
        (ok(), Color::from_rgba8(0x12, 0x16, 0x10, 255), p.lang.start())
    };
    p.fill_round(pad, btn_y, layout.toggle.2, btn_h, 11.0, btn_fill);
    let lw = p.text_width(label, 15.0);
    p.text(
        pad + (layout.toggle.2 - lw) / 2.0,
        btn_y + 26.0,
        15.0,
        label,
        btn_text,
    );

    layout.refresh = (pad + 180.0, btn_y, 92.0, btn_h);
    p.fill_round(layout.refresh.0, btn_y, layout.refresh.2, btn_h, 11.0, panel());
    p.stroke_round(
        layout.refresh.0,
        btn_y,
        layout.refresh.2,
        btn_h,
        11.0,
        line(),
        1.0,
    );
    let rl = if p.lang.cjk { "设备" } else { "SCAN" };
    let rw2 = p.text_width(rl, 12.0);
    p.text(
        layout.refresh.0 + (layout.refresh.2 - rw2) / 2.0,
        btn_y + 26.0,
        12.0,
        rl,
        dim(),
    );

    for (i, line_txt) in p.lang.hint().lines().enumerate() {
        p.text_right(
            wf - pad,
            btn_y + 16.0 + i as f32 * 15.0,
            10.5,
            line_txt,
            dim(),
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// 线性电平 -> 0..1（-60dB 到 0dB）
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
