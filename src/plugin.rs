//! CLAP 插件的粘合层。
//!
//! 全部 unsafe 都集中在这个文件，逻辑很薄：把音频线程、PipeWire 线程、
//! 界面线程三边接起来。

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    clap_event_header, clap_event_param_value, clap_event_transport, clap_input_events,
    clap_output_events, CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT, CLAP_TRANSPORT_IS_RECORDING,
};
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::clap_plugin_latency;
use clap_sys::ext::params::*;
use clap_sys::ext::state::*;
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::host::clap_host;
use clap_sys::id::CLAP_INVALID_ID;
use clap_sys::plugin::*;
use clap_sys::process::{clap_process, CLAP_PROCESS_CONTINUE};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::string_sizes::{CLAP_NAME_SIZE, CLAP_PATH_SIZE};
use clap_sys::version::CLAP_VERSION;

use crate::capture::Capture;
use crate::ctl::{zoom_index, Ctl, ZOOMS};
use crate::gui::{self, Gui};
use crate::ring::{Frame, Ring};

fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TAP_DEBUG").is_some())
}

macro_rules! dbg_log {
    ($($t:tt)*) => {
        if debug_enabled() {
            eprintln!("tap: {}", format!($($t)*));
        }
    };
}

/// 调试用：TAP_DEBUG_TONE=1 时输出 440 Hz 正弦，忽略 PipeWire 抓音。
/// 用途：在宿主里验证「插件的输出到底有没有被采纳」（排除抓音链路的干扰）。
fn debug_tone_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TAP_DEBUG_TONE").is_some())
}

const PARAM_GAIN: u32 = 1;
const PARAM_ZOOM: u32 = 2;
const PARAM_MODE: u32 = 3;

/// 缓冲目标：60 ms。既是延迟补偿值，也是漂移补偿的平衡点。
const FILL_SECS: f64 = 0.060;

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
    id: b"dev.tap.capture\0".as_ptr() as *const c_char,
    name: b"TAP\0".as_ptr() as *const c_char,
    vendor: b"shienze\0".as_ptr() as *const c_char,
    url: b"https://github.com/77-223255/neilu\0".as_ptr() as *const c_char,
    manual_url: b"https://github.com/77-223255/neilu#readme\0".as_ptr() as *const c_char,
    support_url: b"https://github.com/77-223255/neilu/issues\0".as_ptr() as *const c_char,
    version: concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char,
    description: b"System audio (default sink monitor) straight into your DAW track\0".as_ptr()
        as *const c_char,
    features: FEATURES.0.as_ptr(),
};

// ---------------------------------------------------------------- 插件对象

#[repr(C)]
pub struct Tap {
    clap: clap_plugin,
    host: *const clap_host,
    host_gui: *const clap_host_gui,
    // 各个扩展的静态 vtable（生命周期必须 >= 插件）
    ext_audio_ports: clap_plugin_audio_ports,
    ext_params: clap_plugin_params,
    ext_state: clap_plugin_state,
    ext_latency: clap_plugin_latency,
    ext_gui: clap_plugin_gui,

    ctl: Arc<Ctl>,
    capture: Arc<Capture>,
    ring: Arc<Ring>,

    gui: Option<Box<Gui>>,

    sr: f64,
    prev: Frame,
    cur: Frame,
    phase: f64,
    tone_phase: f64,
    hold: [f32; 2],
    hold_age: [u32; 2],
    /// 宿主有没有给过走带信息（没给过就别 gate，不然插件永远静音）
    transport_seen: bool,
    recording: bool,
    gated: bool,
}

impl Tap {
    fn new(host: *const clap_host) -> *const clap_plugin {
        let capture = Arc::new(Capture::new());
        let ring = capture.ring();
        let ctl = Arc::new(Ctl::new());

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
            host_gui: std::ptr::null(),
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
            ext_latency: clap_plugin_latency { get: Some(la_get) },
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
            tone_phase: 0.0,
            hold: [0.0; 2],
            hold_age: [0; 2],
            transport_seen: false,
            recording: false,
            gated: false,
        });

        let ptr = &mut *boxed as *mut Self as *mut c_void;
        boxed.clap.plugin_data = ptr;
        Box::into_raw(boxed) as *const clap_plugin
    }

    fn request_callback(&self) {
        if self.host.is_null() {
            return;
        }
        unsafe {
            if let Some(f) = (*self.host).request_callback {
                f(self.host);
            }
        }
    }

    /// 把缩放档位对应的窗口尺寸报给宿主（必须在主线程）
    fn apply_zoom(&self) {
        if self.host.is_null() || self.host_gui.is_null() {
            return;
        }
        let (w, h) = gui::size_for(self.ctl.zoom_scale());
        unsafe {
            if let Some(f) = (*self.host_gui).request_resize {
                f(self.host, w, h);
            }
        }
    }

    /// 主线程上的杂活：状态回填 + 缩放落地。
    fn tick(&mut self) {
        self.ctl.set_running(self.capture.is_running());
        self.ctl.set_error(self.capture.error());
        self.ctl.set_sr(self.sr as u32);
        if self.ctl.take_zoom_dirty() {
            self.apply_zoom();
            self.request_callback();
        }
    }
}

unsafe fn me<'a>(p: *const clap_plugin) -> &'a mut Tap {
    &mut *((*p).plugin_data as *mut Tap)
}

// ---------------------------------------------------------------- 基本接口

unsafe extern "C" fn s_init(p: *const clap_plugin) -> bool {
    let s = me(p);
    dbg_log!("init");
    if s.host.is_null() {
        return true;
    }
    if let Some(get) = (*s.host).get_extension {
        let id = b"clap.gui\0".as_ptr() as *const c_char;
        let ptr = get(s.host, id) as *const clap_host_gui;
        if !ptr.is_null() {
            s.host_gui = ptr;
        }
    }
    true
}

unsafe extern "C" fn s_destroy(p: *const clap_plugin) {
    drop(Box::from_raw((*p).plugin_data as *mut Tap));
}

unsafe extern "C" fn s_activate(p: *const clap_plugin, sr: f64, _min: u32, _max: u32) -> bool {
    let s = me(p);
    dbg_log!("activate sr={sr}");
    s.sr = if sr > 0.0 { sr } else { 48000.0 };
    s.ctl.set_sr(s.sr as u32);
    s.phase = 1.0;
    s.prev = Frame::default();
    s.cur = Frame::default();
    s.ring.reset();
    true
}

unsafe extern "C" fn s_deactivate(p: *const clap_plugin) {
    dbg_log!("deactivate");
    me(p).capture.stop();
}

unsafe extern "C" fn s_start_processing(p: *const clap_plugin) -> bool {
    let s = me(p);
    dbg_log!("start_processing");
    // 宿主要开始处理了：开抓默认输出，把积压清掉
    s.capture.start();
    s.ring.reset();
    s.phase = 1.0;
    s.tick();
    true
}

unsafe extern "C" fn s_stop_processing(p: *const clap_plugin) {
    dbg_log!("stop_processing");
    me(p).capture.stop();
}

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
    // 注意：返回 0 是 CLAP_PROCESS_ERROR —— 宿主会把输出 buffer 丢掉。
    // 成功必须返回 CLAP_PROCESS_CONTINUE(1)，这是之前「轨道没声音」的元凶之一。
    if proc.is_null() {
        return CLAP_PROCESS_CONTINUE;
    }
    let s = me(p);
    let pr = &*proc;

    read_events(s, pr.in_events);

    if pr.audio_outputs_count == 0 || pr.frames_count == 0 || pr.audio_outputs.is_null() {
        return CLAP_PROCESS_CONTINUE;
    }

    // 输出 buffer 必须用可变引用：宿主从不清 constant_mask，
    // 而我们写的是逐样本变化的数据，必须自己把它打成 0。
    // 不清的后果：宿主以为每个 block 是恒定值，只读第 0 个样本铺满整块，
    // 我们捕获到的音频就被「采样保持」成噪声。
    let ob: &mut clap_audio_buffer = &mut *(pr.audio_outputs as *mut clap_audio_buffer);
    ob.constant_mask = 0;
    ob.latency = 0;
    if ob.channel_count == 0 || ob.data32.is_null() {
        return CLAP_PROCESS_CONTINUE;
    }
    let out_l = *ob.data32;
    if out_l.is_null() {
        return CLAP_PROCESS_CONTINUE;
    }
    let out_r = if ob.channel_count > 1 {
        *ob.data32.add(1)
    } else {
        out_l
    };

    let frames = pr.frames_count as usize;
    let host_sr = if s.sr > 0.0 { s.sr } else { 48000.0 };

    // 输入：这条轨上原本要流过去的东西（比如已经录好的片段）。
    // 我们是插入式效果，必须把它原样带下去，否则重放会没声。
    let (in_l, in_r) = if pr.audio_inputs_count > 0 && !pr.audio_inputs.is_null() {
        let ib = &*pr.audio_inputs;
        let l = if ib.data32.is_null() || ib.channel_count == 0 {
            std::ptr::null()
        } else {
            *ib.data32
        };
        let r = if l.is_null() {
            std::ptr::null()
        } else if ib.channel_count > 1 {
            *ib.data32.add(1)
        } else {
            l
        };
        (l, r)
    } else {
        (std::ptr::null(), std::ptr::null())
    };

    // ── 只在录音时输出 ──
    // 录完之后重放工程时，如果插件还在往外吐实时声音，就会和录好的片段叠在一起，
    // 听起来像「宿主又放了一遍」。所以默认只在 transport 处于录音状态时输出。
    if let Some(flags) = transport_flags(pr) {
        s.transport_seen = true;
        s.recording = flags & CLAP_TRANSPORT_IS_RECORDING != 0;
    }
    s.ctl.set_recording(s.recording);
    let gated = s.ctl.mode() == 0 && s.transport_seen && !s.recording;
    if gated {
        // 不录音：只透传输入，不叠加实时捕获
        for i in 0..frames {
            let l = if in_l.is_null() { 0.0 } else { *in_l.add(i) };
            let r = if in_r.is_null() { l } else { *in_r.add(i) };
            let (l, r) = if out_r != out_l { (l, r) } else { (0.5 * (l + r), 0.0) };
            *out_l.add(i) = l;
            if out_r != out_l {
                *out_r.add(i) = r;
            }
        }
        // 环形缓冲不动它，电平自然回落
        for ch in 0..2 {
            s.ctl.set_meter(ch, s.ctl.meter(ch) * 0.86);
            s.ctl.set_hold(ch, s.ctl.hold(ch) * 0.90);
        }
        s.gated = true;
        ob.constant_mask = 0;
        return CLAP_PROCESS_CONTINUE;
    }
    if s.gated {
        // 刚开始录：丢掉空档期间攒下的旧音频
        s.gated = false;
        s.phase = 1.0;
        s.prev = Frame::default();
        s.cur = Frame::default();
    }

    if debug_tone_enabled() {
        // 调试音：只用来验证宿主有没有采纳我们的输出
        for i in 0..frames {
            let v = (0.4 * (std::f64::consts::TAU * 440.0 * s.tone_phase).sin()) as f32;
            s.tone_phase += 1.0 / host_sr;
            *out_l.add(i) = v;
            if out_r != out_l {
                *out_r.add(i) = v;
            }
        }
        s.tone_phase = s.tone_phase.fract();
        ob.constant_mask = 0;
        return CLAP_PROCESS_CONTINUE;
    }

    let gain = 10f32.powf(s.ctl.gain_db() / 20.0);
    let card_sr = s.capture.rate();

    // 声卡速率 / 宿主速率 = 每输出一帧要凑多少输入帧
    let ratio = if card_sr > 0 {
        card_sr as f64 / host_sr
    } else {
        1.0
    };
    // 时钟漂移补偿：按缓冲填充量微调播放速率，避免长期累积成爆音。
    // 目标至少两块多，块越小延迟越低（回授之外的“二次声”也越不明显）。
    let target = (frames as f64 * 2.0 + host_sr * 0.008)
        .clamp(host_sr * 0.020, host_sr * 0.100);
    let ring: *const Ring = &*s.ring;
    let mut avail = (*ring).available() as f64;
    if avail > target * 4.0 {
        // 宿主停过一阵（积压到半秒以上）：跳到目标填充量，别放隔夜菜
        let mut skip = (avail - target) as usize;
        let mut tmp = Frame::default();
        while skip > 0 && (*ring).read_one(&mut tmp) {
            skip -= 1;
        }
        avail = (*ring).available() as f64;
    }
    let err = ((avail - target) / target.max(1.0)).clamp(-0.5, 0.5);
    let step = ratio * (1.0 + err * 0.02);

    let mut prev = s.prev;
    let mut cur = s.cur;
    let mut phase = s.phase;
    let mut peak = [0f32; 2];

    for i in 0..frames {
        // ratio > 1 时一帧输出可能要吃掉两帧输入，所以必须是 while 不是 if
        while phase >= 1.0 {
            prev = cur;
            if !(*ring).read_one(&mut cur) {
                cur = Frame::default();
            }
            phase -= 1.0;
        }
        let t = phase.clamp(0.0, 1.0) as f32;
        let il = if in_l.is_null() { 0.0 } else { *in_l.add(i) };
        let ir = if in_r.is_null() { il } else { *in_r.add(i) };
        let l = il + (prev.l + (cur.l - prev.l) * t) * gain;
        let r = ir + (prev.r + (cur.r - prev.r) * t) * gain;
        phase += step;

        peak[0] = peak[0].max(l.abs());
        peak[1] = peak[1].max(r.abs());

        if out_r != out_l {
            *out_l.add(i) = l;
            *out_r.add(i) = r;
        } else {
            *out_l.add(i) = 0.5 * (l + r);
        }
    }

    s.prev = prev;
    s.cur = cur;
    s.phase = phase;

    // 电平表 + 峰值保持
    for ch in 0..2 {
        let previous = s.ctl.meter(ch);
        let shown = if peak[ch] > previous {
            peak[ch]
        } else {
            previous * 0.86
        };
        s.ctl.set_meter(ch, shown);
        if shown >= s.hold[ch] {
            s.hold[ch] = shown;
            s.hold_age[ch] = 0;
        } else {
            s.hold_age[ch] = s.hold_age[ch].saturating_add(frames as u32);
            if s.hold_age[ch] as f64 > host_sr * 1.1 {
                s.hold[ch] *= 0.92;
            }
        }
        s.ctl.set_hold(ch, s.hold[ch]);
    }

    s.ctl.set_fill(s.ring.available() as u32);
    s.ctl.set_dropped(s.capture.dropped_frames());
    ob.constant_mask = 0;
    CLAP_PROCESS_CONTINUE
}

unsafe fn read_events(s: &mut Tap, events: *const clap_input_events) {
    if events.is_null() {
        return;
    }
    let ev = &*events;
    let (Some(size), Some(get)) = (ev.size, ev.get) else {
        return;
    };
    for i in 0..size(ev) {
        let h: *const clap_event_header = get(ev, i);
        if h.is_null() || (*h).type_ != CLAP_EVENT_PARAM_VALUE {
            continue;
        }
        let pv = &*(h as *const clap_event_param_value);
        apply_param(s, pv.param_id, pv.value);
    }
}

/// 读走带 flags：优先 process.transport，其次输入事件里的 transport 事件
unsafe fn transport_flags(pr: &clap_process) -> Option<u32> {
    if !pr.transport.is_null() {
        return Some((*pr.transport).flags);
    }
    let ev = pr.in_events;
    if ev.is_null() {
        return None;
    }
    let ev = &*ev;
    let (Some(size), Some(get)) = (ev.size, ev.get) else {
        return None;
    };
    for i in 0..size(ev) {
        let h = get(ev, i);
        if h.is_null() || (*h).type_ != CLAP_EVENT_TRANSPORT {
            continue;
        }
        return Some((*(h as *const clap_event_transport)).flags);
    }
    None
}

fn apply_param(s: &mut Tap, id: u32, value: f64) {
    match id {
        PARAM_GAIN => s.ctl.set_gain_db(value as f32),
        PARAM_ZOOM => {
            let idx = value.max(0.0).round() as i32;
            if idx != s.ctl.zoom() {
                s.ctl.set_zoom(idx);
                s.request_callback();
            }
        }
        PARAM_MODE => s.ctl.set_mode(value.max(0.0).round() as i32),
        _ => {}
    }
}

// ---------------------------------------------------------------- 扩展

unsafe extern "C" fn s_get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return std::ptr::null();
    }
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
    if index != 0 || info.is_null() {
        return false;
    }
    let i = &mut *info;
    i.id = if is_input { 0 } else { 1 };
    write_name(&mut i.name, if is_input { "Input" } else { "Capture" });
    i.flags = CLAP_AUDIO_PORT_IS_MAIN;
    i.channel_count = 2;
    i.port_type = CLAP_PORT_STEREO.as_ptr();
    // 不做 in-place：输入我们不读，输出写自己的内存最稳
    i.in_place_pair = CLAP_INVALID_ID;
    true
}

fn write_name(dst: &mut [c_char; CLAP_NAME_SIZE], s: &str) {
    for (i, b) in s.as_bytes().iter().take(CLAP_NAME_SIZE - 1).enumerate() {
        dst[i] = *b as c_char;
    }
}

// ---- params

unsafe extern "C" fn pa_count(_p: *const clap_plugin) -> u32 {
    3
}

unsafe extern "C" fn pa_get_info(
    _p: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    if info.is_null() {
        return false;
    }
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
            i.id = PARAM_ZOOM;
            write_name(&mut i.name, "UI Zoom");
            i.flags = CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM;
            i.min_value = 0.0;
            i.max_value = (ZOOMS.len() - 1) as f64;
            i.default_value = 1.0;
            true
        }
        2 => {
            i.id = PARAM_MODE;
            write_name(&mut i.name, "Output");
            i.flags = CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM;
            i.min_value = 0.0;
            i.max_value = 1.0;
            i.default_value = 0.0;
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn pa_get_value(p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if out.is_null() {
        return false;
    }
    let s = me(p);
    match id {
        PARAM_GAIN => *out = s.ctl.gain_db() as f64,
        PARAM_ZOOM => *out = s.ctl.zoom() as f64,
        PARAM_MODE => *out = s.ctl.mode() as f64,
        _ => return false,
    }
    true
}

unsafe extern "C" fn pa_value_to_text(
    _p: *const clap_plugin,
    id: u32,
    value: f64,
    out: *mut c_char,
    size: u32,
) -> bool {
    let text = match id {
        PARAM_GAIN => format!("{value:+.1} dB"),
        PARAM_ZOOM => {
            let idx = value.max(0.0).round() as usize;
            let z = ZOOMS.get(idx).copied().unwrap_or(1.0);
            format!("{:.0} %", z * 100.0)
        }
        PARAM_MODE => {
            if value.max(0.0).round() as i32 == 0 {
                "Rec only".to_string()
            } else {
                "Always".to_string()
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
    if text.is_null() || out.is_null() {
        return false;
    }
    let txt = std::ffi::CStr::from_ptr(text).to_string_lossy().to_string();
    match id {
        PARAM_GAIN => {
            let cleaned = txt.trim_end_matches("dB").trim();
            match cleaned.parse::<f64>() {
                Ok(v) => {
                    *out = v.clamp(-24.0, 24.0);
                    true
                }
                Err(_) => false,
            }
        }
        PARAM_ZOOM => {
            let cleaned = txt.trim_end_matches('%').trim();
            match cleaned.parse::<f64>() {
                Ok(v) => {
                    *out = zoom_index((v / 100.0) as f32) as f64;
                    true
                }
                Err(_) => false,
            }
        }
        PARAM_MODE => {
            let t = txt.trim().to_ascii_lowercase();
            if t.starts_with("rec") {
                *out = 0.0;
                true
            } else if t.starts_with("always") || t.starts_with("live") {
                *out = 1.0;
                true
            } else {
                false
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
    let s = me(p);
    read_events(s, input);
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
    if stream.is_null() {
        return false;
    }
    let s = me(p);
    let text = format!(
        "gain_db={}\nzoom={}\nmode={}\n",
        s.ctl.gain_db(),
        s.ctl.zoom(),
        s.ctl.mode()
    );
    write_stream(stream, text.as_bytes())
}

unsafe extern "C" fn st_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    if stream.is_null() {
        return false;
    }
    let s = me(p);
    let mut buf = [0u8; 512];
    let mut len = 0usize;
    while len < buf.len() {
        let mut chunk = [0u8; 128];
        let n = read_stream(stream, &mut chunk);
        if n <= 0 {
            break;
        }
        let n = (n as usize).min(buf.len() - len);
        buf[len..len + n].copy_from_slice(&chunk[..n]);
        len += n;
    }
    let text = String::from_utf8_lossy(&buf[..len]).to_string();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("gain_db=") {
            if let Ok(g) = v.trim().parse::<f32>() {
                s.ctl.set_gain_db(g);
            }
        } else if let Some(v) = line.strip_prefix("zoom=") {
            if let Ok(z) = v.trim().parse::<i32>() {
                s.ctl.set_zoom(z);
                s.request_callback();
            }
        } else if let Some(v) = line.strip_prefix("mode=") {
            if let Ok(m) = v.trim().parse::<i32>() {
                s.ctl.set_mode(m);
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
    (s.sr * FILL_SECS) as u32
}

// ---- gui

unsafe extern "C" fn gu_is_api_supported(
    _p: *const clap_plugin,
    api: *const c_char,
    is_floating: bool,
) -> bool {
    !is_floating && api_matches(api, b"x11")
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
        s.gui = Some(Box::new(Gui::new(
            s.ctl.clone(),
            s.capture.clone(),
            s.host,
        )));
    }
    true
}

unsafe extern "C" fn gu_destroy(p: *const clap_plugin) {
    me(p).gui = None;
}

unsafe extern "C" fn gu_set_scale(_p: *const clap_plugin, _scale: f64) -> bool {
    true
}

unsafe extern "C" fn gu_get_size(p: *const clap_plugin, w: *mut u32, h: *mut u32) -> bool {
    if w.is_null() || h.is_null() {
        return false;
    }
    let s = me(p);
    let (gw, gh) = gui::size_for(s.ctl.zoom_scale());
    *w = gw;
    *h = gh;
    true
}

unsafe extern "C" fn gu_can_resize(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn gu_get_resize_hints(
    _p: *const clap_plugin,
    hints: *mut clap_gui_resize_hints,
) -> bool {
    if hints.is_null() {
        return false;
    }
    let h = &mut *hints;
    // 只接受自己的档位，不让宿主自由拉
    h.can_resize_horizontally = false;
    h.can_resize_vertically = false;
    h.preserve_aspect_ratio = false;
    h.aspect_ratio_width = 0;
    h.aspect_ratio_height = 0;
    true
}

unsafe extern "C" fn gu_adjust_size(p: *const clap_plugin, w: *mut u32, h: *mut u32) -> bool {
    if w.is_null() || h.is_null() {
        return false;
    }
    let s = me(p);
    let (gw, gh) = gui::size_for(s.ctl.zoom_scale());
    *w = gw;
    *h = gh;
    true
}

unsafe extern "C" fn gu_set_size(p: *const clap_plugin, w: u32, h: u32) -> bool {
    if w == 0 || h == 0 {
        return false;
    }
    let s = me(p);
    s.ctl.set_zoom_quiet(zoom_index(w as f32 / gui::BASE_W));
    if let Some(g) = s.gui.as_mut() {
        g.resize(w, h);
    }
    true
}

unsafe extern "C" fn gu_set_parent(p: *const clap_plugin, window: *const clap_window) -> bool {
    if window.is_null() {
        return false;
    }
    let win = &*window;
    if !api_matches(win.api, b"x11") {
        return false;
    }
    let parent = win.specific.x11 as u32;
    let s = me(p);
    match s.gui.as_mut() {
        Some(g) => g.attach(parent),
        None => false,
    }
}

unsafe extern "C" fn gu_set_transient(_p: *const clap_plugin, _window: *const clap_window) -> bool {
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
        dbg_log!(
            "create_plugin: id mismatch, want {} got {}",
            want.to_string_lossy(),
            got.to_string_lossy()
        );
        return std::ptr::null();
    }
    dbg_log!("create_plugin ok");
    Tap::new(host)
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
