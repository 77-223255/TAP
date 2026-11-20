//! CLAP 插件的粘合层。
//!
//! 全部 unsafe 都集中在这个文件，逻辑很薄：把音频线程、PipeWire 线程、
//! 界面线程三边接起来。

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    clap_event_header, clap_event_param_value, clap_input_events, clap_output_events,
    CLAP_EVENT_PARAM_VALUE,
};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::clap_plugin_latency;
use clap_sys::ext::params::*;
use clap_sys::ext::state::*;
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::host::clap_host;
use clap_sys::plugin::*;
use clap_sys::process::clap_process;
use clap_sys::string_sizes::{CLAP_NAME_SIZE, CLAP_PATH_SIZE};
use clap_sys::version::CLAP_VERSION;

use crate::capture::Capture;
use crate::ctl::Ctl;
use crate::gui::{Gui, HEIGHT, WIDTH};
use crate::ring::{Frame, Ring};

const PARAM_GAIN: u32 = 1;
const PARAM_SOURCE: u32 = 2;

// ---------------------------------------------------------------- 描述符

struct FeaturePtrs([*const c_char; 3]);
unsafe impl Sync for FeaturePtrs {}

static FEATURES: FeaturePtrs = FeaturePtrs([
    b"audio-effect\0".as_ptr() as *const c_char,
    b"utility\0".as_ptr() as *const c_char,
    std::ptr::null(),
]);

static DESC: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: b"dev.neilu.capture\0".as_ptr() as *const c_char,
    name: b"NeiLu Capture\0".as_ptr() as *const c_char,
    vendor: b"neilu\0".as_ptr() as *const c_char,
    url: b"https://github.com/77-223255/neilu\0".as_ptr() as *const c_char,
    manual_url: b"https://github.com/77-223255/neilu#readme\0".as_ptr() as *const c_char,
    support_url: b"https://github.com/77-223255/neilu/issues\0".as_ptr() as *const c_char,
    version: b"0.1.0\0".as_ptr() as *const c_char,
    description: b"PipeWire system audio capture straight into your DAW track\0".as_ptr()
        as *const c_char,
    features: FEATURES.0.as_ptr(),
};

// ---------------------------------------------------------------- 插件对象

#[repr(C)]
pub struct NeiLu {
    clap: clap_plugin,
    host: *const clap_host,
    // 各个扩展的静态 vtable（生命周期必须 >= 插件）
    ext_audio_ports: clap_plugin_audio_ports,
    ext_params: clap_plugin_params,
    ext_state: clap_plugin_state,
    ext_latency: clap_plugin_latency,
    ext_gui: clap_plugin_gui,

    ctl: Arc<Ctl>,
    capture: Capture,
    ring: Arc<Ring>,

    gui: Option<Box<Gui>>,

    sr: f64,
    prev: Frame,
    cur: Frame,
    phase: f64,
    rate: f64,
    hold: [f32; 2],
    hold_age: [u32; 2],
    autostarted: bool,
}

impl NeiLu {
    fn new(host: *const clap_host) -> *const clap_plugin {
        let capture = Capture::new();
        let ring = capture.ring();
        let ctl = Arc::new(Ctl::new());
        ctl.capacity
            .store(ring.capacity() as u32, std::sync::atomic::Ordering::Relaxed);

        let mut boxed = Box::new(Self {
            clap: clap_plugin {
                desc: &DESC,
                plugin_data: std::ptr::null_mut(),
                init: Some(s_init),
                destroy: Some(s_destroy),
                activate: Some(s_activate),
                deactivate: Some(s_deactivate),
                start_processing: Some(s_start_processing),
                stop_processing: Some(s_stop_processing),
                reset: Some(s_reset),
                process: Some(s_process),
                get_extension: Some(s_get_extension),
                on_main_thread: Some(s_on_main_thread),
            },
            host,
            ext_audio_ports: clap_plugin_audio_ports {
                count: Some(ap_count),
                get: Some(ap_get),
            },
            ext_params: clap_plugin_params {
                count: Some(pa_count),
                get_info: Some(pa_get_info),
                get_value: Some(pa_get_value),
                value_to_text: Some(pa_value_to_text),
                text_to_value: Some(pa_text_to_value),
                flush: Some(pa_flush),
            },
            ext_state: clap_plugin_state {
                save: Some(st_save),
                load: Some(st_load),
            },
            ext_latency: clap_plugin_latency {
                get: Some(la_get),
            },
            ext_gui: clap_plugin_gui {
                is_api_supported: Some(gu_is_api_supported),
                get_preferred_api: Some(gu_get_preferred_api),
                create: Some(gu_create),
                destroy: Some(gu_destroy),
                set_scale: Some(gu_set_scale),
                get_size: Some(gu_get_size),
                can_resize: Some(gu_can_resize),
                get_resize_hints: Some(gu_get_resize_hints),
                adjust_size: Some(gu_adjust_size),
                set_size: Some(gu_set_size),
                set_parent: Some(gu_set_parent),
                set_transient: Some(gu_set_transient),
                suggest_title: Some(gu_suggest_title),
                show: Some(gu_show),
                hide: Some(gu_hide),
            },
            ctl,
            capture,
            ring,
            gui: None,
            sr: 48000.0,
            prev: Frame::default(),
            cur: Frame::default(),
            phase: 1.0,
            rate: 1.0,
            hold: [0.0; 2],
            hold_age: [0; 2],
            autostarted: false,
        });

        let ptr = &mut *boxed as *mut Self as *mut c_void;
        boxed.clap.plugin_data = ptr;
        Box::into_raw(boxed) as *const clap_plugin
    }

    /// 主线程上的杂活：把 GUI 的请求落到 PipeWire，把状态回填给 GUI。
    fn tick(&mut self) {
        // 第一次 tick 就自动开始抓系统默认输出，插上就能录
        if !self.autostarted {
            self.autostarted = true;
            self.capture.start(None, self.sr as u32);
        }
        if let Some(sink) = self.ctl.take_start() {
            self.ctl.sr.store(self.sr as u32, std::sync::atomic::Ordering::Relaxed);
            self.capture.start(sink.as_deref(), self.sr as u32);

            self.ring.reset();
        }
        if self.ctl.take_stop() {
            self.capture.stop();
        }
        self.ctl.set_running(self.capture.is_running());
        self.ctl.set_sinks(self.capture.sinks());
        self.ctl
            .fill
            .store(self.ring.available() as u32, std::sync::atomic::Ordering::Relaxed);
        self.ctl
            .dropped
            .store(self.capture.dropped_frames(), std::sync::atomic::Ordering::Relaxed);
    }
}

unsafe fn me<'a>(p: *const clap_plugin) -> &'a mut NeiLu {
    &mut *((*p).plugin_data as *mut NeiLu)
}

// ---------------------------------------------------------------- 基本接口

unsafe extern "C" fn s_init(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn s_destroy(p: *const clap_plugin) {
    drop(Box::from_raw((*p).plugin_data as *mut NeiLu));
}

unsafe extern "C" fn s_activate(p: *const clap_plugin, sr: f64, _min: u32, _max: u32) -> bool {
    let s = me(p);
    s.sr = if sr > 0.0 { sr } else { 48000.0 };
    s.ctl
        .sr
        .store(s.sr as u32, std::sync::atomic::Ordering::Relaxed);
    s.phase = 1.0;
    true
}

unsafe extern "C" fn s_deactivate(_p: *const clap_plugin) {}

unsafe extern "C" fn s_start_processing(p: *const clap_plugin) -> bool {
    let s = me(p);
    s.tick();
    true
}

unsafe extern "C" fn s_stop_processing(_p: *const clap_plugin) {}

unsafe extern "C" fn s_reset(p: *const clap_plugin) {
    let s = me(p);
    s.ring.reset();
    s.phase = 1.0;
}

unsafe extern "C" fn s_on_main_thread(p: *const clap_plugin) {
    me(p).tick();
}

// ---------------------------------------------------------------- 处理

unsafe extern "C" fn s_process(p: *const clap_plugin, proc: *const clap_process) -> i32 {
    let s = me(p);
    let pr = &*proc;

    // 参数自动化
    if !pr.in_events.is_null() {
        let ev = &*pr.in_events;
        if let (Some(size), Some(get)) = (ev.size, ev.get) {
            for i in 0..size(ev) {
                let h = get(ev, i);
                if h.is_null() || (*h).type_ != CLAP_EVENT_PARAM_VALUE {
                    continue;
                }
                let pv = &*(h as *const clap_event_param_value);
                apply_param(s, pv.param_id, pv.value);
            }
        }
    }

    if pr.audio_outputs_count == 0 || pr.frames_count == 0 {
        return 0;
    }
    let ob = &*pr.audio_outputs;
    if ob.channel_count == 0 || ob.data32.is_null() {
        return 0;
    }

    let frames = pr.frames_count as usize;
    let gain = 10f32.powf(s.ctl.gain_db() / 20.0);
    let ring = s.ring.clone();

    // 时钟漂移补偿：按缓冲填充量微调播放速率，避免长期累积成爆音
    let target = (s.sr * 0.1) as f64;
    let avail = ring.available() as f64;
    let err = ((avail - target) / target.max(1.0)).clamp(-0.5, 0.5);
    s.rate = 1.0 + err * 0.02;

    let mut peak = [0f32; 2];
    for i in 0..frames {
        if s.phase >= 1.0 {
            s.prev = s.cur;
            if !ring.read_one(&mut s.cur) {
                s.cur = Frame::default();
            }
            s.phase -= 1.0;
        }
        let t = s.phase as f32;
        let l = s.prev.l + (s.cur.l - s.prev.l) * t;
        let r = s.prev.r + (s.cur.r - s.prev.r) * t;
        s.phase += s.rate;

        let lo = l * gain;
        let ro = r * gain;
        peak[0] = peak[0].max(lo.abs());
        peak[1] = peak[1].max(ro.abs());

        let ch0 = *ob.data32;
        *ch0.add(i) = lo;
        if ob.channel_count > 1 {
            let ch1 = *ob.data32.add(1);
            *ch1.add(i) = ro;
        }
    }

    // 电平表 + 峰值保持
    for ch in 0..2 {
        let prev = s.ctl.meter(ch);
        let shown = if peak[ch] > prev {
            peak[ch]
        } else {
            prev * 0.86
        };
        s.ctl.set_meter(ch, shown);
        if shown >= s.hold[ch] {
            s.hold[ch] = shown;
            s.hold_age[ch] = 0;
        } else {
            s.hold_age[ch] = s.hold_age[ch].saturating_add(frames as u32);
            if s.hold_age[ch] as f64 > s.sr * 1.1 {
                s.hold[ch] *= 0.92;
            }
        }
        s.ctl.set_hold(ch, s.hold[ch]);
    }
    s.ctl
        .fill
        .store(ring.available() as u32, std::sync::atomic::Ordering::Relaxed);
    s.ctl.set_running(s.capture.is_running());

    0
}

fn apply_param(s: &mut NeiLu, id: u32, value: f64) {
    match id {
        PARAM_GAIN => s.ctl.set_gain_db(value as f32),
        PARAM_SOURCE => {
            let idx = value.max(0.0) as i32;
            if idx != s.ctl.source() {
                s.ctl.set_source(idx);
                let sink = if idx == 0 {
                    None
                } else {
                    s.ctl.sinks().get((idx - 1) as usize).map(|x| x.name.clone())
                };
                s.ctl.request_start(sink);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- 扩展

unsafe extern "C" fn s_get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    let s = me(p);
    let name = std::ffi::CStr::from_ptr(id).to_bytes();
    match name {
        b"clap.audio-ports" => &s.ext_audio_ports as *const _ as *const c_void,
        b"clap.params" => &s.ext_params as *const _ as *const c_void,
        b"clap.state" => &s.ext_state as *const _ as *const c_void,
        b"clap.latency" => &s.ext_latency as *const _ as *const c_void,
        b"clap.gui" => &s.ext_gui as *const _ as *const c_void,
        b"clap.thread-check" => std::ptr::null(),
        _ => std::ptr::null(),
    }
}

// ---- audio ports

unsafe extern "C" fn ap_count(_p: *const clap_plugin, _is_input: bool) -> u32 {
    1
}

unsafe extern "C" fn ap_get(
    _p: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index != 0 {
        return false;
    }
    let i = &mut *info;
    i.id = if is_input { 0 } else { 1 };
    write_name(&mut i.name, if is_input { "Input" } else { "Capture" });
    i.flags = CLAP_AUDIO_PORT_IS_MAIN;
    i.channel_count = 2;
    i.port_type = CLAP_PORT_STEREO.as_ptr();
    i.in_place_pair = !is_input as u32;
    true
}

fn write_name(dst: &mut [c_char; CLAP_NAME_SIZE], s: &str) {
    for (i, b) in s.as_bytes().iter().take(CLAP_NAME_SIZE - 1).enumerate() {
        dst[i] = *b as c_char;
    }
}

// ---- params

unsafe extern "C" fn pa_count(_p: *const clap_plugin) -> u32 {
    2
}

unsafe extern "C" fn pa_get_info(
    _p: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    let i = &mut *info;
    i.module = [0; CLAP_PATH_SIZE];
    i.cookie = std::ptr::null_mut();
    i.flags = 0;
    match index {
        0 => {
            i.id = PARAM_GAIN;
            write_name(&mut i.name, "Gain");
            i.min_value = -24.0;
            i.max_value = 24.0;
            i.default_value = 0.0;
            true
        }
        1 => {
            i.id = PARAM_SOURCE;
            write_name(&mut i.name, "Source");
            i.flags = CLAP_PARAM_IS_STEPPED;
            i.min_value = 0.0;
            i.max_value = 32.0;
            i.default_value = 0.0;
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn pa_get_value(_p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    let s = me(_p);
    if out.is_null() {
        return false;
    }
    match id {
        PARAM_GAIN => *out = s.ctl.gain_db() as f64,
        PARAM_SOURCE => *out = s.ctl.source() as f64,
        _ => return false,
    }
    true
}

unsafe extern "C" fn pa_value_to_text(
    p: *const clap_plugin,
    id: u32,
    value: f64,
    out: *mut c_char,
    size: u32,
) -> bool {
    let s = me(p);
    let text = match id {
        PARAM_GAIN => format!("{value:+.1} dB"),
        PARAM_SOURCE => {
            let idx = value.max(0.0) as usize;
            if idx == 0 {
                "System default".to_string()
            } else {
                s.ctl
                    .sinks()
                    .get(idx - 1)
                    .map(|x| x.label.clone())
                    .unwrap_or_else(|| "-".to_string())
            }
        }
        _ => return false,
    };
    write_cstr(out, size, &text);
    true
}

unsafe extern "C" fn pa_text_to_value(
    _p: *const clap_plugin,
    id: u32,
    text: *const c_char,
    out: *mut f64,
) -> bool {
    let s = std::ffi::CStr::from_ptr(text).to_string_lossy().to_string();
    match id {
        PARAM_GAIN => {
            let cleaned = s.trim_end_matches("dB").trim();
            match cleaned.parse::<f64>() {
                Ok(v) => {
                    *out = v.clamp(-24.0, 24.0);
                    true
                }
                Err(_) => false,
            }
        }
        PARAM_SOURCE => {
            let me_ = me(_p);
            match me_.ctl.sinks().iter().position(|x| x.label == s) {
                Some(i) => {
                    *out = (i + 1) as f64;
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

unsafe extern "C" fn pa_flush(
    p: *const clap_plugin,
    input: *const clap_input_events,
    _output: *const clap_output_events,
) {
    if input.is_null() {
        return;
    }
    let s = me(p);
    let ev = &*input;
    if let (Some(size), Some(get)) = (ev.size, ev.get) {
        for i in 0..size(ev) {
            let h: *const clap_event_header = get(ev, i);
            if h.is_null() || (*h).type_ != CLAP_EVENT_PARAM_VALUE {
                continue;
            }
            let pv = &*(h as *const clap_event_param_value);
            apply_param(s, pv.param_id, pv.value);
        }
    }
}

fn write_cstr(dst: *mut c_char, size: u32, s: &str) {
    if dst.is_null() || size == 0 {
        return;
    }
    let n = (size as usize - 1).min(s.len());
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr() as *const c_char, dst, n);
        *dst.add(n) = 0;
    }
}

// ---- state

unsafe extern "C" fn st_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    let s = me(p);
    if stream.is_null() {
        return false;
    }
    let sink = if s.ctl.source() <= 0 {
        "auto".to_string()
    } else {
        s.ctl
            .sinks()
            .get((s.ctl.source() - 1).max(0) as usize)
            .map(|x| x.name.clone())
            .unwrap_or_else(|| "auto".to_string())
    };
    let text = format!("sink={}\ngain_db={}\n", sink, s.ctl.gain_db());
    write_stream(stream, text.as_bytes())
}

unsafe extern "C" fn st_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    let s = me(p);
    if stream.is_null() {
        return false;
    }
    let mut buf = [0u8; 1024];
    let mut len = 0usize;
    while len < buf.len() {
        let mut chunk = [0u8; 256];
        let n = read_stream(stream, &mut chunk);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        buf[len..len + n].copy_from_slice(&chunk[..n]);
        len += n;
    }
    let text = String::from_utf8_lossy(&buf[..len]).to_string();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("sink=") {
            let v = v.trim().to_string();
            if v == "auto" || v.is_empty() {
                s.ctl.set_source(0);
                s.ctl.request_start(None);
            } else if let Some(i) = s.ctl.sinks().iter().position(|x| x.name == v) {
                s.ctl.set_source((i + 1) as i32);
                s.ctl.request_start(Some(v));
            }
        } else if let Some(v) = line.strip_prefix("gain_db=") {
            if let Ok(g) = v.trim().parse::<f32>() {
                s.ctl.set_gain_db(g);
            }
        }
    }
    true
}

unsafe fn write_stream(stream: *const clap_ostream, mut data: &[u8]) -> bool {
    let s = &*stream;
    let Some(write) = s.write else { return false };
    while !data.is_empty() {
        let n = write(stream, data.as_ptr() as *const c_void, data.len() as u64);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

unsafe fn read_stream(stream: *const clap_istream, dst: &mut [u8]) -> i64 {
    let s = &*stream;
    let Some(read) = s.read else { return -1 };
    read(stream, dst.as_mut_ptr() as *mut c_void, dst.len() as u64)
}

// ---- latency

unsafe extern "C" fn la_get(p: *const clap_plugin) -> u32 {
    let s = me(p);
    (s.sr * 0.1) as u32
}

// ---- gui

unsafe extern "C" fn gu_is_api_supported(
    _p: *const clap_plugin,
    api: *const c_char,
    is_floating: bool,
) -> bool {
    is_floating && api.is_null() && false || (!is_floating && api_matches(api, b"x11"))
}

fn api_matches(api: *const c_char, name: &[u8]) -> bool {
    if api.is_null() {
        return false;
    }
    unsafe { std::ffi::CStr::from_ptr(api).to_bytes() == name }
}

static API_X11: &[u8] = b"x11\0";

unsafe extern "C" fn gu_get_preferred_api(
    _p: *const clap_plugin,
    api: *mut *const c_char,
    is_floating: *mut bool,
) -> bool {
    if !api.is_null() {
        *api = API_X11.as_ptr() as *const c_char;
    }
    if !is_floating.is_null() {
        *is_floating = false;
    }
    true
}

unsafe extern "C" fn gu_create(p: *const clap_plugin, api: *const c_char, is_floating: bool) -> bool {
    if is_floating || !api_matches(api, b"x11") {
        return false;
    }
    let s = me(p);
    if s.gui.is_none() {
        s.gui = Some(Box::new(Gui::new(s.ctl.clone())));
    }
    true
}

unsafe extern "C" fn gu_destroy(p: *const clap_plugin) {
    me(p).gui = None;
}

unsafe extern "C" fn gu_set_scale(_p: *const clap_plugin, _scale: f64) -> bool {
    true
}

unsafe extern "C" fn gu_get_size(
    _p: *const clap_plugin,
    w: *mut u32,
    h: *mut u32,
) -> bool {
    if w.is_null() || h.is_null() {
        return false;
    }
    *w = WIDTH;
    *h = HEIGHT;
    true
}

unsafe extern "C" fn gu_can_resize(_p: *const clap_plugin) -> bool {
    false
}

unsafe extern "C" fn gu_get_resize_hints(
    _p: *const clap_plugin,
    hints: *mut clap_gui_resize_hints,
) -> bool {
    if hints.is_null() {
        return false;
    }
    let h = &mut *hints;
    h.can_resize_horizontally = false;
    h.can_resize_vertically = false;
    h.preserve_aspect_ratio = false;
    h.aspect_ratio_width = 0;
    h.aspect_ratio_height = 0;
    true
}

unsafe extern "C" fn gu_adjust_size(
    _p: *const clap_plugin,
    w: *mut u32,
    h: *mut u32,
) -> bool {
    if w.is_null() || h.is_null() {
        return false;
    }
    *w = WIDTH;
    *h = HEIGHT;
    true
}

unsafe extern "C" fn gu_set_size(p: *const clap_plugin, w: u32, h: u32) -> bool {
    let s = me(p);
    if let Some(g) = s.gui.as_mut() {
        g.resize(w, h);
    }
    true
}

unsafe extern "C" fn gu_set_parent(p: *const clap_plugin, window: *const clap_window) -> bool {
    if window.is_null() {
        return false;
    }
    let s = me(p);
    let win = &*window;
    if !api_matches(win.api, b"x11") {
        return false;
    }
    let parent = win.specific.x11 as u32;
    match s.gui.as_mut() {
        Some(g) => g.attach(parent),
        None => false,
    }
}

unsafe extern "C" fn gu_set_transient(
    _p: *const clap_plugin,
    _window: *const clap_window,
) -> bool {
    true
}

unsafe extern "C" fn gu_suggest_title(_p: *const clap_plugin, _title: *const c_char) {}

unsafe extern "C" fn gu_show(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn gu_hide(_p: *const clap_plugin) -> bool {
    true
}

// ---------------------------------------------------------------- 工厂

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn factory_descriptor(
    _f: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        &DESC
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn factory_create(
    _f: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if id.is_null() {
        return std::ptr::null();
    }
    let want = std::ffi::CStr::from_ptr(DESC.id);
    let got = std::ffi::CStr::from_ptr(id);
    if want != got {
        return std::ptr::null();
    }
    NeiLu::new(host)
}

pub static PLUGIN_FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
};

// ---------------------------------------------------------------- 入口

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(factory_id: *const c_char) -> *const c_void {
    if factory_id.is_null() {
        return std::ptr::null();
    }
    let id = std::ffi::CStr::from_ptr(factory_id);
    if id == clap_sys::factory::plugin_factory::CLAP_PLUGIN_FACTORY_ID {
        &PLUGIN_FACTORY as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};

// 让 clap_audio_buffer 的导入不被裁剪，顺便避免 unused 警告
#[allow(dead_code)]
fn _touch(b: *const clap_audio_buffer) -> u32 {
    unsafe { (*b).channel_count }
}
