# 在 REAPER 里用 TAP

## 一次性的设置

### 1. 让 REAPER 只录不响

插件管不到宿主的输出路由，所以这一步在 REAPER 里做：

1. 点轨道的 **`Route`** 按钮
2. 取消勾选 **`Master send`**

这条轨就不会再往 master 送声音：录音时你只听见源本身（没有第二份），
而 `Record: output` 的取样点在发送到 master 之前，所以照样能录到。

想听已经录好的片段时，再把这个勾打开。

### 2. 录音模式

右键轨道的录音按钮（或右键轨道 → `Record: output`）：

- 选 **`Record: output (stereo)`**
- **再点一次**选子模式：
  - `post fader`：受推子和静音影响
  - `post-fx pre-fader`：推荐，插件之后、推子之前
  - `pre fx`：插件之前

### 3. 插件

轨道插 **TAP** → 面板空白处右键 → **`Capture`** 打勾（默认是关的）。

灯/电平圈动起来就说明在抓了。`Zoom` 也在同一个右键菜单里。

## 录

按录音键。轨道上出现片段即可。

- **不要开轨道的监听**：源本来就在响，监听只会再叠一份晚几十毫秒的副本
- 录音时如果你听见两遍，八成是 `Master send` 没关
- 抓的是所有应用的播放流（排除 REAPER 自己），所以浏览器、播放器、游戏的声音都会进来

## 导出

**方式一：Render**

1. 选中那条轨
2. `File → Render…`
3. `Source` 选 **`Selected tracks (stems)`**，`Bounds` 选 `Entire project` 或 `Time selection`，
   `Output` 选 `WAV` / `24 bit`
4. 填目录和文件名 → **`Render 1 file`**

渲染是离线跑的，不录音时插件只透传，所以不会把当时的系统声音混进去。

**方式二：就地出 stem**

右键轨道 → `Render/freeze tracks` → `Render tracks to stereo stem tracks (and mute originals)`。
会多出一条 `[stem]` 轨，音频就在里面；只要文件的话去工程的 Render/Media 目录拿。

**方式三：只处理某个片段**

右键片段 → `Apply track/take FX to items as new take`，再 `Glue items`。

## 排查

| 现象 | 原因 |
|---|---|
| 插件电平圈不动 | `Capture` 没开；或者没有应用在放声音 |
| 有电平但轨道没声音 | 插件的 `Capture` 开了、但轨道 `Record: output` 选成了 `input` |
| 录下来是麦克风的声音 | 录音模式不是 `Record: output`，而是 `input (audio)` |
| 听见两份（回声） | `Master send` 没关，或者开了轨道监听 |
| 播放录好的片段没声音 | 轨道 `Master send` 关着（听的时候打开） |
| 报错文本 | 面板上会出现橙色错误提示（例如 PipeWire 连不上） |

调试：

```bash
TAP_DEBUG=1 reaper        # 打印 CLAP 回调与抓音错误
TAP_DEBUG_TONE=1 reaper   # 忽略抓音，插件直接吐 440Hz（判断宿主有没有采纳输出）
```

## 为什么抓 monitor 不行

`monitor` 里包含宿主自己的输出，抓它会形成 插件输出 → 声卡 → monitor → 插件输入 的
增益为 1 的闭环，几秒内就啸叫。所以 TAP 抓的是**每个应用的播放流节点**，
并且排除本进程，结构上不可能成环。
