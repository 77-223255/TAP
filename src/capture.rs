//! PipeWire 捕获线程。
//!
//! 一个后台线程持有自己的 MainLoop，主线程通过 mpsc 发命令。
//! 音频数据只经过 [`Ring`]，从不在锁里穿过。
//!
//! 两件事：
//! * 抓的是**系统默认输出（sink）的 monitor**：`stream.capture.sink = true`。
//!   不需要虚拟声卡，也不需要用户选设备。
//! * **不**强制采样率：让 PipeWire 按声卡当前的图速率协商，
//!   协商结果通过 `param_changed` 回读，插件再重采样到宿主速率。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;

use crate::ring::Ring;

enum Cmd {
    Start,
    Stop,
    Shutdown,
}

pub struct Capture {
    tx: Sender<Cmd>,
    ring: Arc<Ring>,
    running: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
    rate: Arc<AtomicU32>,
    error: Arc<Mutex<String>>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let ring = Arc::new(Ring::new(1 << 15));
        let running = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicU64::new(0));
        let rate = Arc::new(AtomicU32::new(0));
        let error = Arc::new(Mutex::new(String::new()));

        let th = {
            let ring = ring.clone();
            let running = running.clone();
            let received = received.clone();
            let rate = rate.clone();
            let error = error.clone();
            thread::Builder::new()
                .name("tap-pipewire".into())
                .spawn(move || thread_main(rx, ring, running, received, rate, error))
                .ok()
        };

        Self {
            tx,
            ring,
            running,
            received,
            rate,
            error,
            thread: th,
        }
    }

    pub fn start(&self) {
        let _ = self.tx.send(Cmd::Start);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn ring(&self) -> Arc<Ring> {
        self.ring.clone()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 声卡那边协商出来的采样率（0 = 还没协商出来）
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> String {
        self.error.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn dropped_frames(&self) -> u64 {
        self.ring.dropped()
    }

    pub fn received_frames(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 线程内部

fn thread_main(
    rx: Receiver<Cmd>,
    ring: Arc<Ring>,
    running: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
    rate: Arc<AtomicU32>,
    error: Arc<Mutex<String>>,
) {
    let fail = |msg: String| {
        eprintln!("tap: {msg}");
        if let Ok(mut g) = error.lock() {
            *g = msg;
        }
    };

    pw::init();

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => return fail(format!("无法创建 PipeWire mainloop: {e}")),
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(e) => return fail(format!("无法创建 PipeWire context: {e}")),
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => return fail(format!("无法连接 PipeWire: {e}")),
    };

    let mut active: Option<Active> = None;
    loop {
        let mut shutdown = false;
        loop {
            match rx.try_recv() {
                Ok(Cmd::Start) => {
                    active = None;
                    running.store(false, Ordering::Relaxed);
                    ring.reset();
                    rate.store(0, Ordering::Relaxed);
                    if let Ok(mut g) = error.lock() {
                        g.clear();
                    }
                    match make_stream(&core, &ring, &running, &received, &rate) {
                        Ok(pair) => {
                            active = Some(Active {
                                stream: pair.0,
                                _listener: pair.1,
                            });
                            running.store(true, Ordering::Relaxed);
                        }
                        Err(e) => fail(format!("打开默认输出的 monitor 失败: {e}")),
                    }
                }
                Ok(Cmd::Stop) => {
                    active = None;
                    running.store(false, Ordering::Relaxed);
                }
                Ok(Cmd::Shutdown) => shutdown = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => shutdown = true,
            }
            if shutdown {
                break;
            }
        }
        if shutdown {
            drop(active);
            running.store(false, Ordering::Relaxed);
            return;
        }
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(10)));
    }
}

/// 活着的流 + 它的监听器，靠 Drop 收尾
struct Active {
    #[allow(dead_code)]
    stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<()>,
}

fn make_stream(
    core: &pw::core::CoreRc,
    ring: &Arc<Ring>,
    running: &Arc<AtomicBool>,
    received: &Arc<AtomicU64>,
    rate: &Arc<AtomicU32>,
) -> Result<(pw::stream::StreamRc, pw::stream::StreamListener<()>), pw::Error> {
    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => "tap-capture",
        "node.description" => "TAP capture",
        // 关键：抓默认 sink 的 monitor，而不是麦克风
        "stream.capture.sink" => "true",
    };

    let stream = pw::stream::StreamRc::new(core.clone(), "tap", props)?;

    let listener = {
        let ring = ring.clone();
        let running = running.clone();
        let received = received.clone();
        let rate = rate.clone();
        stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let running = running.clone();
                move |_s, _d, _old, new| {
                    match new {
                        pw::stream::StreamState::Error(e) => {
                            eprintln!("tap: 流错误: {e}");
                        }
                        pw::stream::StreamState::Paused => {
                            running.store(false, Ordering::Relaxed);
                        }
                        pw::stream::StreamState::Streaming => {
                            running.store(true, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
            })
            .param_changed({
                let rate = rate.clone();
                move |_s, _d, id, param| {
                    if id != pw::spa::param::ParamType::Format.as_raw() {
                        return;
                    }
                    let Some(pod) = param else { return };
                    let mut info = pw::spa::param::audio::AudioInfoRaw::new();
                    if info.parse(pod).is_ok() {
                        let r = info.rate();
                        if r > 0 {
                            rate.store(r, Ordering::Relaxed);
                        }
                    }
                }
            })
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let (offset, size) = {
                    let c = d.chunk();
                    (c.offset() as usize, c.size() as usize)
                };
                if size < 8 {
                    return;
                }
                let Some(bytes) = d.data() else { return };
                let end = offset.saturating_add(size).min(bytes.len());
                if end <= offset + 8 {
                    return;
                }
                let raw = &bytes[offset..end];
                // PipeWire 给的是 f32 交错立体声，映射内存天然 4 字节对齐
                let floats: &[f32] =
                    unsafe { std::slice::from_raw_parts(raw.as_ptr().cast(), raw.len() / 4) };
                ring.write_interleaved(floats);
                received.fetch_add((raw.len() / 8) as u64, Ordering::Relaxed);
            })
            .register()?
    };

    // 只固定格式和声道数，**不固定采样率**：让 PipeWire 跟声卡走
    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_channels(2);

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();

    let mut params = [pw::spa::pod::Pod::from_bytes(&values).expect("format pod")];

    stream.connect(
        pw::spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    Ok((stream, listener))
}
