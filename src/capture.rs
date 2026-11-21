//! PipeWire 捕获线程。
//!
//! 思路和 OBS 的「应用音频采集」一样：**把每个应用的播放流节点直接连进我们的
//! 输入口，不碰 sink 的 monitor**。
//!
//! 为什么这么做：抓 sink 的 monitor 会把宿主（DAW）自己的输出也抓进来，
//! 于是 插件输出 → 声卡 → monitor → 插件输入 形成增益为 1 的环路，
//! 立刻啸叫。按应用连节点时，宿主的播放流会被显式排除，
//! **结构上不可能成环**，也就不需要虚拟声卡、不需要关监听、不需要静音轨道。
//!
//! 采样率仍然不强制：让我们自己的流跟声卡当前的图速率协商，
//! 协商结果通过 `param_changed` 回读，插件再重采样到宿主速率。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    apps: Arc<Mutex<Vec<String>>>,
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
        let apps = Arc::new(Mutex::new(Vec::new()));

        let th = {
            let ring = ring.clone();
            let running = running.clone();
            let received = received.clone();
            let rate = rate.clone();
            let error = error.clone();
            let apps = apps.clone();
            thread::Builder::new()
                .name("tap-pipewire".into())
                .spawn(move || thread_main(rx, ring, running, received, rate, error, apps))
                .ok()
        };

        Self {
            tx,
            ring,
            running,
            received,
            rate,
            error,
            apps,
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

    /// 当前连着的声音来自哪些应用（给界面显示）
    pub fn apps(&self) -> Vec<String> {
        self.apps.lock().map(|v| v.clone()).unwrap_or_default()
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

// ---------------------------------------------------------------- 图

#[derive(Default)]
struct Graph {
    /// 我们自己的节点 id（0 = 还没连上）
    our_node: u32,
    nodes: HashMap<u32, NodeInfo>,
    ports: HashMap<u32, PortInfo>,
    /// client id -> 进程 pid
    clients: HashMap<u32, u32>,
    links: HashMap<(u32, u32), pw::link::Link>,
    dirty: bool,
}

struct NodeInfo {
    /// media.class
    class: String,
    /// 给界面看的名字
    label: String,
    /// 属于哪个 PipeWire client（节点自己的 props 里经常没有 pid，
    /// 要顺着 client.id 去 Client 对象上找）
    client_id: u32,
}

struct PortInfo {
    node: u32,
    out: bool,
    idx: u32,
}

impl Graph {
    /// 节点所属进程的 pid（认不出来就当作不认识）
    fn pid_of(&self, node: u32) -> Option<u32> {
        let n = self.nodes.get(&node)?;
        self.clients.get(&n.client_id).copied()
    }

    /// 目标节点：所有应用播放流，但排除本进程（宿主自己）。
    /// 排除宿主是这套设计的核心：宿主的输出永远不会被自己抓回来，
    /// 所以在结构上不可能形成啸叫回路。
    fn is_target(&self, id: u32) -> bool {
        match self.nodes.get(&id) {
            Some(n) => {
                n.class == "Stream/Output/Audio"
                    && matches!(self.pid_of(id), Some(pid) if pid != std::process::id())
            }
            None => false,
        }
    }

    fn our_inputs(&self) -> Vec<u32> {
        let mut v: Vec<(u32, u32)> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == self.our_node && !p.out)
            .map(|(id, p)| (p.idx, *id))
            .collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    fn app_outputs(&self, node: u32) -> Vec<u32> {
        let mut v: Vec<(u32, u32)> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == node && p.out)
            .map(|(id, p)| (p.idx, *id))
            .collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// 想要建立的 (输出口, 输入口) 对
    fn desired(&self) -> Vec<(u32, u32)> {
        if self.our_node == 0 {
            return Vec::new();
        }
        let ins = self.our_inputs();
        if ins.is_empty() {
            return Vec::new();
        }
        let mut want = Vec::new();
        let mut targets: Vec<u32> = self
            .nodes
            .keys()
            .copied()
            .filter(|id| self.is_target(*id))
            .collect();
        targets.sort();
        for node in targets {
            let outs = self.app_outputs(node);
            if outs.is_empty() {
                continue;
            }
            if outs.len() >= ins.len() {
                for (o, i) in outs.iter().zip(ins.iter()) {
                    want.push((*o, *i));
                }
            } else {
                // 单声道应用：复制到左右两路（保持居中）
                for o in outs.iter() {
                    for i in ins.iter() {
                        want.push((*o, *i));
                    }
                }
            }
        }
        want
    }

    fn sync(&mut self, core: &pw::core::CoreRc) {
        let want = self.desired();
        // 拆掉不再需要的
        let stale: Vec<(u32, u32)> = self
            .links
            .keys()
            .copied()
            .filter(|k| !want.contains(k))
            .collect();
        for k in stale {
            self.links.remove(&k);
        }
        // 建立缺的
        for (out_port, in_port) in want {
            if self.links.contains_key(&(out_port, in_port)) {
                continue;
            }
            let props = properties! {
                *pw::keys::LINK_OUTPUT_PORT => out_port.to_string(),
                *pw::keys::LINK_INPUT_PORT => in_port.to_string(),
                "object.linger" => "false",
            };
            match core.create_object::<pw::link::Link>("link-factory", &props) {
                Ok(link) => {
                    self.links.insert((out_port, in_port), link);
                }
                Err(e) => eprintln!("tap: 建立连接失败 ({out_port}->{in_port}): {e}"),
            }
        }
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
    apps: Arc<Mutex<Vec<String>>>,
) {
    pw::init();

    let fail = |msg: String| {
        eprintln!("tap: {msg}");
        if let Ok(mut g) = error.lock() {
            *g = msg;
        }
    };

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
    let registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => return fail(format!("无法获取 registry: {e}")),
    };

    // 跟踪节点 / 端口，随时知道有哪些应用在放声音
    let graph: Arc<Mutex<Graph>> = Arc::new(Mutex::new(Graph::default()));
    let _listener = registry
        .add_listener_local()
        .global({
            let graph = graph.clone();
            move |obj| {
                let mut g = graph.lock().unwrap();
                match obj.type_ {
                    pw::types::ObjectType::Node => {
                        let Some(props) = obj.props else { return };
                        let class = props.get("media.class").unwrap_or("").to_string();
                        let label = props
                            .get("application.name")
                            .or_else(|| props.get("node.description"))
                            .or_else(|| props.get("node.name"))
                            .unwrap_or("?")
                            .to_string();
                        let client_id = props
                            .get("client.id")
                            .and_then(|p| p.parse::<u32>().ok())
                            .unwrap_or(0);
                        g.nodes.insert(
                            obj.id,
                            NodeInfo {
                                class,
                                label,
                                client_id,
                            },
                        );
                    }
                    pw::types::ObjectType::Client => {
                        let Some(props) = obj.props else { return };
                        let pid = props
                            .get("application.process.id")
                            .or_else(|| props.get("pipewire.sec.pid"))
                            .and_then(|p| p.parse::<u32>().ok());
                        match pid {
                            Some(pid) => {
                                g.clients.insert(obj.id, pid);
                            }
                            None => {
                                g.clients.remove(&obj.id);
                            }
                        }
                    }
                    pw::types::ObjectType::Port => {
                        let Some(props) = obj.props else { return };
                        let node = props
                            .get("node.id")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0);
                        let idx = props
                            .get("port.id")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(0);
                        let out = props.get("port.direction") == Some("out");
                        g.ports.insert(obj.id, PortInfo { node, out, idx });
                    }
                    _ => return,
                }
                g.dirty = true;
            }
        })
        .global_remove({
            let graph = graph.clone();
            move |id| {
                let mut g = graph.lock().unwrap();
                g.nodes.remove(&id);
                g.ports.remove(&id);
                g.clients.remove(&id);
                g.dirty = true;
            }
        })
        .register();

    let mut active: Option<Active> = None;
    let mut last_sync = Instant::now() - Duration::from_secs(1);

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
                    {
                        let mut g = graph.lock().unwrap();
                        g.links.clear();
                        g.our_node = 0;
                    }
                    match make_stream(&core, &ring, &running, &received, &rate) {
                        Ok(pair) => {
                            active = Some(Active {
                                stream: pair.0,
                                _listener: pair.1,
                            });
                        }
                        Err(e) => fail(format!("创建捕获流失败: {e}")),
                    }
                    last_sync = Instant::now() - Duration::from_secs(1);
                }
                Ok(Cmd::Stop) => {
                    active = None;
                    running.store(false, Ordering::Relaxed);
                    let mut g = graph.lock().unwrap();
                    g.links.clear();
                    g.our_node = 0;
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

        // 连上节点、跟随应用出现/消失
        if let Some(a) = active.as_ref() {
            let mut g = graph.lock().unwrap();
            let id = a.stream.node_id();
            if id != 0 {
                g.our_node = id;
            }
            if g.dirty || last_sync.elapsed() > Duration::from_millis(250) {
                g.sync(&core);
                g.dirty = false;
                drop(g);
                last_sync = Instant::now();
                // 更新界面上的应用名
                let g = graph.lock().unwrap();
                let mut names: Vec<String> = g
                    .nodes
                    .keys()
                    .copied()
                    .filter(|n| g.is_target(*n))
                    .filter_map(|n| g.nodes.get(&n).map(|x| x.label.clone()))
                    .collect();
                names.sort();
                names.dedup();
                drop(g);
                if let Ok(mut a) = apps.lock() {
                    if *a != names {
                        *a = names;
                    }
                }
            }
        } else if let Ok(mut a) = apps.lock() {
            a.clear();
        }

        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(20)));
    }
}

/// 活着的流 + 它的监听器，靠 Drop 收尾
struct Active {
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
        // 不自动连到默认设备：下面的 Graph 会把每个应用节点接进来
        *pw::keys::NODE_AUTOCONNECT => "false",
    };

    let stream = pw::stream::StreamRc::new(core.clone(), "tap", props)?;

    let listener = {
        let ring = ring.clone();
        let received = received.clone();
        let rate = rate.clone();
        stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let running = running.clone();
                move |_s, _d, _old, new| match new {
                    pw::stream::StreamState::Error(e) => eprintln!("tap: 流错误: {e}"),
                    pw::stream::StreamState::Streaming => running.store(true, Ordering::Relaxed),
                    pw::stream::StreamState::Paused | pw::stream::StreamState::Unconnected => {
                        running.store(false, Ordering::Relaxed)
                    }
                    _ => {}
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
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    Ok((stream, listener))
}
