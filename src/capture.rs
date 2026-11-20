//! PipeWire 捕获线程。
//!
//! 结构：一个后台线程持有自己的 MainLoop，主线程通过 mpsc 发命令。
//! 音频数据只经过 `Ring`，从不在锁里穿过。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use pipewire as pw;
use pw::properties::properties;

use crate::ring::Ring;

#[derive(Clone, Debug, PartialEq)]
pub struct SinkInfo {
    pub name: String,
    pub label: String,
}

enum Cmd {
    /// sink = None 表示跟随系统默认输出
    Start { sink: Option<String>, rate: u32 },
    Stop,
    Shutdown,
}

pub struct Capture {
    tx: Sender<Cmd>,
    ring: Arc<Ring>,
    sinks: Arc<Mutex<Vec<SinkInfo>>>,
    running: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let ring = Arc::new(Ring::new(1 << 15));
        let sinks = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicU64::new(0));

        let th = {
            let ring = ring.clone();
            let sinks = sinks.clone();
            let running = running.clone();
            let received = received.clone();
            thread::Builder::new()
                .name("neilu-pipewire".into())
                .spawn(move || thread_main(rx, ring, sinks, running, received))
                .ok()
        };

        Self {
            tx,
            ring,
            sinks,
            running,
            received,
            thread: th,
        }
    }

    pub fn start(&self, sink: Option<&str>, rate: u32) {
        let _ = self.tx.send(Cmd::Start {
            sink: sink.map(|s| s.to_string()),
            rate,
        });
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn ring(&self) -> Arc<Ring> {
        self.ring.clone()
    }

    pub fn sinks(&self) -> Vec<SinkInfo> {
        self.sinks.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn received_frames(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    pub fn dropped_frames(&self) -> u64 {
        self.ring.dropped()
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
    sinks: Arc<Mutex<Vec<SinkInfo>>>,
    running: Arc<AtomicBool>,
    received: Arc<AtomicU64>,
) {
    pw::init();

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("neilu: 无法创建 PipeWire mainloop: {e}");
            return;
        }
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("neilu: 无法创建 PipeWire context: {e}");
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("neilu: 无法连接 PipeWire: {e}");
            return;
        }
    };
    let registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("neilu: 无法获取 registry: {e}");
            return;
        }
    };

    // 设备列表：注册表事件驱动，随时更新
    let _reg_listener = {
        let sinks = sinks.clone();
        registry
            .add_listener_local()
            .global(move |obj| {
                let Some(props) = obj.props else { return };
                if props.get("media.class") != Some("Audio/Sink") {
                    return;
                }
                let Some(name) = props.get("node.name") else {
                    return;
                };
                // nick 通常最短最好认（"HDMI 1"），没有才退回完整描述
                let label = props
                    .get("node.nick")
                    .or_else(|| props.get("node.description"))
                    .unwrap_or(name)
                    .to_string();
                let mut v = sinks.lock().unwrap();
                match v.iter_mut().find(|s| s.name == name) {
                    Some(e) => e.label = label,
                    None => {
                        v.push(SinkInfo {
                            name: name.to_string(),
                            label,
                        });
                        v.sort_by(|a, b| a.label.cmp(&b.label));
                    }
                }
            })
            .register()
    };

    // 先跑一会儿，让初始的 global 事件到齐
    for _ in 0..6 {
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(std::time::Duration::from_millis(20)));
    }

    let mut active: Option<Active> = None;
    loop {
        let mut shutdown = false;
        loop {
            match rx.try_recv() {
                Ok(Cmd::Start { sink, rate }) => {
                    active = None;
                    running.store(false, Ordering::Relaxed);
                    ring.reset();
                    let label = sink.clone().unwrap_or_else(|| "默认输出".into());
                    match make_stream(&core, &ring, &running, &received, sink.as_deref(), rate) {
                        Ok(pair) => {
                            active = Some(Active {
                                stream: pair.0,
                                _listener: pair.1,
                            });
                            running.store(active.is_some(), Ordering::Relaxed);
                        }
                        Err(e) => eprintln!("neilu: 打开流失败 ({label}): {e}"),
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
            .iterate(pw::loop_::Timeout::Finite(std::time::Duration::from_millis(20)));
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
    sink: Option<&str>,
    rate: u32,
) -> Result<(pw::stream::StreamRc, pw::stream::StreamListener<()>), pw::Error> {
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };
    match sink {
        // 明确指定设备：连它的 monitor
        Some(name) => {
            props.insert("target.object", format!("{name}.monitor"));
        }
        // 跟随系统默认输出
        None => {
            props.insert("stream.capture.sink", "true");
        }
    }
    props.insert(*pw::keys::NODE_NAME, "neilu-capture".to_string());
    props.insert("node.description", "NeiLu 内录".to_string());

    let stream = pw::stream::StreamRc::new(core.clone(), "neilu", props)?;

    let listener = {
        let ring = ring.clone();
        let running = running.clone();
        let received = received.clone();
        stream
            .add_local_listener_with_user_data(())
            .state_changed(move |_, _, _, new| {
                let ok = !matches!(new, pw::stream::StreamState::Error(_));
                running.store(ok, Ordering::Relaxed);
            })
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let bytes = d.chunk().size() as usize;
                let Some(samples) = d.data() else { return };
                if bytes < 8 {
                    return;
                }
                // PipeWire 给的是 f32 交错立体声，映射内存天然 4 字节对齐
                let floats: &[f32] =
                    unsafe { std::slice::from_raw_parts(samples.as_ptr().cast(), bytes / 4) };
                ring.write_interleaved(floats);
                received.fetch_add((bytes / 8) as u64, Ordering::Relaxed);
            })
            .register()?
    };

    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(rate);
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
