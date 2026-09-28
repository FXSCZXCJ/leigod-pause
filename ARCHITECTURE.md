# 架构说明 · leigod-pause

> 这份文档写给**第一次读 Rust 项目的人**：不需要会 Rust，出现的术语都会先解释。
> 读完你应该能大致说清楚：这个程序由哪几块组成、数据怎么在它们之间流动、为什么有些地方写得那么绕。
>
> 想直接上手改代码，看第 10 节「常见修改任务」；只想用工具，看 [README](README.md)。

---

## 0. 一分钟了解

**它是什么**：一个常驻 Windows 托盘的小工具。挂机玩游戏的场景下，游戏关掉后自动把雷神加速器的计时**暂停**（不浪费剩余时长），下次开机/开游戏前再恢复。

**它由什么做的**：

| 部分 | 用的库 | 作用 |
|---|---|---|
| 界面 | `eframe` / `egui` | 原生窗口 + 立即模式 UI（不是网页，也不用装运行时） |
| 托盘图标 | `tray-icon` | 任务栏右下角图标与右键菜单 |
| 网络请求 | `reqwest`（阻塞模式） | 调用雷神的 HTTP 接口 |
| 进程扫描 | `sysinfo` | 每秒看一遍系统里跑着什么程序 |
| Windows 系统功能 | `windows` | 关机消息、剪贴板、注册表、托盘窗口、通知身份等 |

**它不做什么**：不注入游戏、不修改雷神客户端、不模拟点击，只调用官方网页版自己用的那几个 HTTP 接口。

---

## 1. 先讲人话：三条主线

程序所有行为都可以归到三条主线上。

### 1.1 自动暂停（核心价值）

```
   [ 扫描进程 ]
        │
        ├─ 发现游戏 → 状态=「在游戏中」                        （什么都不做，让加速器正常工作）
        │
        └─ 游戏一个都没有了 → 状态=「宽限倒计时 180 秒」
                 │
                 ├─ 倒计时里游戏又启动了 → 回到「在游戏中」（取消暂停，不打扰）
                 │
                 └─ 倒计时走完还是没游戏 → 调接口暂停计时 → 状态=「无游戏」
```

宽限 180 秒是给"游戏重启/切服"留的缓冲，可在设置里调 10~600 秒。

**每次要暂停前，都会先查一次账号状态**：如果本来就是"已暂停"，就直接跳过、不重复调接口、不弹提示（这是用户明确要求的行为）。

### 1.2 token 续期（账号登录态）

雷神的接口靠 `account_token` 鉴权，它会过期。程序提供 **5 个入口**拿到新 token，它们最终都汇到同两个函数上：

```
 ① 界面手动粘贴 token ─────────┐
 ② 界面短信验证码登录 ─────────┤
 ③ HTTP 接口 POST /token/code ─┼─→ actions::submit_code()   ─┐
 ④ 命名管道写入验证码 ─────────┘                             ├─→ 写入 config.ini
 ⑤ 剪贴板自动识别 token ─────────→ actions::apply_token()  ─┘   + 通知监控线程
```

写入新 token 后，监控线程会**立刻补执行之前挂起的暂停请求**（比如因为 token 失效没暂停成功的那次）。

### 1.3 关机前强制暂停

```
Windows 通知"要关机了"（WM_QUERYENDSESSION）
        ↓
独立的小窗口收到消息（不依赖界面）
        ↓
同步调用暂停接口（2 秒超时 × 最多 2 次），抢在系统结束进程前完成
        ↓
返回"允许关机"
```

---

## 2. 目录地图

```
leigod-pause/
├─ Cargo.toml           依赖清单（装了什么库、Windows 功能开了哪些）
├─ build.rs             构建脚本：把 assets/leigod.ico 嵌进 exe
├─ assets/leigod.ico     程序图标（托盘 + exe 图标）
├─ API_NOTES.md         雷神接口的实测记录（逆向结果，改接口相关代码前必读）
├─ ARCHITECTURE.md      本文
└─ src/
   ├─ main.rs           入口：解析命令行参数、处理控制台输出、起各个线程、拉起界面
   ├─ state.rs          全局共享状态 AppState（线程之间的"黑板"）+ 日志函数
   ├─ config.rs         config.ini 读写 + 开机自启（注册表 / 启动文件夹）
   ├─ actions.rs        高层动作：查询/暂停/恢复/发短信/验证码登录/打开雷神
   ├─ monitor.rs        监控线程：进程扫描 + 状态机（在游戏中 / 宽限 / 空闲）
   ├─ api/
   │  ├─ mod.rs         雷神 HTTP 客户端（双域名 failover、错误码分类）
   │  └─ sign.rs        官方签名算法（当前登录链路用不到，留作备用）
   ├─ code_api.rs       对外接口：HTTP 服务(127.0.0.1) + 命名管道
   ├─ clipboard.rs      剪贴板监听：识别 token → 验证 → 保存
   ├─ gui.rs            主界面（状态 / 设置 / 登录 / 日志 四个页签）
   ├─ tray.rs           托盘图标与右键菜单的创建
   ├─ events.rs         独立线程：通知、托盘菜单事件、托盘点击
   ├─ steam.rs          扫描 Steam 游戏库（libraryfolders.vdf）
   └─ shutdown.rs       关机消息钩子（message-only 窗口）
```

---

## 3. 线程模型：它是怎么"同时做很多事"的

### 3.1 为什么没有 async/await

现代 Rust 网络程序常用 `async/await` + 运行时（tokio）。本项目**故意全部用普通线程（阻塞式）**，原因很实在：

- 要调的系统功能（读注册表、等关机消息、读剪贴板、托盘菜单）**全是阻塞 API**，用线程最直接；
- 线程数量固定（十来个），不涉及高并发，`async` 带来的复杂度（生命周期、运行时依赖）不划算；
- 新手读代码时，`std::thread::spawn` 比 `async fn` + `.await` + 运行时容易理解得多。

一句话：**每个"需要一直等"的事情，独占一个线程。**

### 3.2 线程清单

| 线程名 | 位置 | 干什么 | 什么时候结束 |
|---|---|---|---|
| 主线程 | `main.rs` / `gui.rs` | 跑 egui 界面循环（**窗口隐藏时这个循环会停摆**，见 6.1） | 程序退出 |
| `monitor` | `monitor.rs` | 每秒扫描进程、跑状态机、到点调暂停接口 | 永不 |
| `tray-menu` | `events.rs` | 阻塞等待托盘右键菜单点击 | 永不 |
| `tray-click` | `events.rs` | 阻塞等待托盘图标左键点击 → 显示主窗口 | 永不 |
| `notify` | `events.rs` | 收通知队列 → 弹 Windows 通知 | 永不 |
| `http-api` | `code_api.rs` | 监听 `127.0.0.1:18100`，处理验证码相关请求 | 永不 |
| `pipe-api` | `code_api.rs` | 监听命名管道 `\\.\pipe\leigod-sms-code` | 永不 |
| `shutdown-hook` | `shutdown.rs` | 消息循环，等 `WM_QUERYENDSESSION` | 永不 |
| `clip-watch` | `clipboard.rs` | 消息循环，等系统"剪贴板变了"通知 | 永不 |
| `autostart-fix` | `main.rs` | 启动后修正开机自启入口参数（一次性） | 干完就退 |

### 3.3 线程之间怎么交换数据（4 种手段）

| 手段 | 用在什么地方 | 通俗解释 |
|---|---|---|
| `Arc<AppState>` | 所有线程共享的"黑板" | **Arc** = 引用计数指针，让多个线程都能持有同一份数据的只读句柄（Rust 里允许多方共享但不允许多方同时写） |
| `Mutex<T>` / `RwLock<T>` | 黑板上的字段，如 token、暂停状态、日志缓冲 | **锁**：同一时刻只有一个人能改；`Mutex` 一读一写都独占，`RwLock` 允许多个读但写独占 |
| `AtomicBool` / `AtomicU64` | 开关与计数器，如 `pending_pause`、`grace_remaining`、`token_version` | **原子类型**：不用加锁也能安全地跨线程读写单个值（比锁快） |
| `mpsc::channel` | ① 界面/托盘 → 监控线程的"命令队列" ② 任意线程 → 通知线程的"待弹通知队列" | **通道**：一端 `send` 一端 `recv` 的管道，天然排队，不用自己加锁 |

> 新手最容易困惑的点：为什么到处都是 `.lock().unwrap()`？
> 因为 `Mutex` 必须先"拿锁"才能访问里面的数据，`.unwrap()` 表示"如果锁坏了就崩"（正常不会发生）。

---

## 4. 共享状态 `AppState`（黑板）

定义在 `src/state.rs`。所有线程读同一块黑板，改完立刻对所有线程可见：

| 字段 | 类型 | 谁写 | 谁读 | 含义 |
|---|---|---|---|---|
| `token` | `Mutex<String>` | 所有 token 更新入口 | 调接口前 | 当前 account_token |
| `token_version` | `AtomicU64` | 每次更新 token 都 +1 | monitor | "token 换了"的信号灯，monitor 看它决定要不要补暂停 |
| `token_valid` | `AtomicBool` | `query_info` 成败 | 界面状态页、/status | 最近一次接口调用 token 是否有效 |
| `pause_status` | `Mutex<Option<i64>>` | 查询/暂停/恢复 | 界面、/status | 0=加速中 1=已暂停 |
| `monitor` | `Mutex<MonitorStatus>` | monitor 线程 | 界面、托盘 tooltip | 当前状态机状态 |
| `grace_remaining` | `AtomicU64` | monitor | 界面倒计时 | 宽限剩余秒数 |
| `pending_pause` | `AtomicBool` | 暂停失败时置位 | monitor 重试逻辑 | 是否有"欠着的暂停请求" |
| `notify_tx` | `Mutex<Option<Sender>>` | 启动时注入 | 任意线程 | 往通知线程投递 toast |
| `gui_ctx` / `native_hwnd` | `Mutex<Option<..>>` | 界面初始化后 | 任意线程 | 让别的线程也能"把主窗口叫出来" |
| `log_buf` | `Mutex<VecDeque<String>>` | `log()` | 界面日志页 | 内存里的最近 500 行日志 |

`log()` 这个函数做了两件事：写内存缓冲（界面看）+ 追加到 exe 同目录的 `leigod_rs.log`（排查问题看）。**遇到问题先看这个日志文件**，每次启动还会打印一行「启动模式：…」，很省事。

---

## 5. 一条完整的数据流（照着读一遍就懂整体了）

以"游戏退出 → 自动暂停"为例，标出每一步落在哪个文件：

```
monitor.rs: loop (每 update 秒一轮)
   │
   ├─ ① 配置文件热加载：比对 config.ini 的修改时间，变了就重新读（含 token）
   │
   ├─ ② 收界面/托盘发来的命令（mpsc::try_recv）：立即暂停 / 恢复加速
   │
   ├─ ③ token_version 变了且有待办暂停 → actions::do_pause() 补执行
   │
   ├─ ④ 有欠着的暂停 → 每 15 秒重试一次（网络抖动、token 失效期间）
   │
   ├─ ⑤ sysinfo 扫描进程 → find_game() 匹配（顺序：进程名规则 → 目录规则 → Steam 库）
   │       │
   │       ├─ 匹配到 → mode = InGame，必要时 actions::do_recover() 自动恢复加速
   │       │
   │       └─ 没匹配到 且 上一轮还在游戏里 → mode = Grace{deadline: now + 宽限}
   │                    │
   │                    └─ 宽限到点 → actions::do_pause()
   │                                 │
   │                                 ├─ 先 api::info() 读状态：已暂停 → 静默跳过
   │                                 ├─ api::pause() 成功 → push_notify 弹通知
   │                                 └─ 失败 → pending_pause = true（转 ④ 重试），界面显示"挂起"
   │
   └─ ⑥ 首次循环额外做一次"启动检测"：没游戏在跑且没暂停 → 直接暂停
```

`actions.rs` 是**唯一**碰雷神接口的业务层，界面、HTTP、管道、CLI、监控线程全都调它，所以行为一致（比如"已暂停就跳过"这条规则只需要写一处）。

---

## 6. 界面与托盘的三个坑（绕开它们的设计）

### 6.1 窗口隐藏时，egui 循环会"停摆"

Windows 下窗口被隐藏后，winit（egui 的窗口后端）的 `request_redraw` **不会**唤醒事件循环，`App::update()` 就再也不会被调用。后果：如果托盘菜单事件放在界面循环里处理，**隐藏状态下点托盘毫无反应**（早期版本踩过这个坑）。

**所以**：托盘菜单、托盘点击、系统通知、窗口显示全部放在**独立线程**（`events.rs`），只用原生的 `ShowWindow` + egui 的 `ViewportCommand` 来显示窗口，绝不依赖界面循环还活着。

### 6.2 点 X 是"隐藏到托盘"，不是退出

在 `gui.rs` 的 `update()` 里监听 `close_requested()`，拦截关闭并回一条 `CancelClose` + `Visible(false)`。真正退出走托盘菜单的「退出 / 退出并暂停时长」。

### 6.3 通知必须挂在"应用身份"（AUMID）上

Windows 的 toast 通知要求发送方有一个已注册的 AppUserModelID，否则系统直接丢弃。未打包的 exe 默认没有身份，**借用的身份会显示成它的名字**（早期版本通知标题显示"Windows PowerShell"就是这个原因）。

`events.rs` 的 `ensure_aumid_shortcut()` 用 Shell COM 在开始菜单建一个带 `System.AppUserModel.ID = Leigod.Pause` 的快捷方式，从此通知标题就是「雷神自动暂停」；万一被安全软件拦下，退回 PowerShell 身份（通知仍能弹，只是标题难看）。

---

## 7. 启动方式、开机自启、单实例

### 7.1 启动模式（重要：exe 是 GUI 子系统程序）

程序编译时声明为 **GUI 子系统**（`#![cfg_attr(not(test), windows_subsystem = "windows")]`），所以**双击不会弹控制台黑窗口**。原来的控制台子系统在 Win11 上会拉起 Windows Terminal 窗口，且从进程内部关不掉它（实测），这是必须换子系统的原因。

| 命令行 | 行为 |
|---|---|
| `leigod-pause.exe` | 后台静默进托盘（默认） |
| `--show` / `-w` | 打开主界面；若已在运行，通过跨进程消息请它显示窗口 |
| `--console` / `-c` | 用 `AllocConsole` 额外开一个控制台窗口实时看日志（调试） |
| `--hidden` / `-b` / `--tray` | 显式表示静默（自启入口写的就是它） |

因为 GUI 子系统程序默认没有标准输出，`main.rs::init_stdio()` 做了这套处理：

| 启动场景 | stdout 情况 | 处理 |
|---|---|---|
| 从终端启动 | 句柄是控制台类型 | `AttachConsole(父进程)` 接上终端，`println!` 照常可见 |
| 输出被重定向（`> out.txt`、`| more`） | 句柄是管道/文件 | **沿用父进程句柄，绝不能改成 CONOUT$**（否则输出会丢，踩过） |
| 双击 / 开机自启 | 没有有效句柄 | 重定向到 `NUL`，日志只写文件，避免 `println!` 因句柄无效而 panic |

### 7.2 开机自启

写入的完整命令行是：`"D:\...\leigod-pause.exe" --hidden`（路径带引号，参数表示静默）。

两条路，按顺序尝试：

1. **注册表** `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 的 `LeigodPause` 值（已是目标值就跳过写入，减少安全软件弹窗）；
2. 被拒（`os error 5`，常见于 360 等）→ 回退到 **启动文件夹快捷方式** `%APPDATA%\...\Startup\leigod-pause.lnk`（用 PowerShell 的 `WScript.Shell` 创建，带 `Arguments`）。

启动时 `refresh_autostart_params()` 会给**已存在且路径匹配**的旧入口补上 `--hidden`，不会新建入口、也不会改路径（避免另一个副本运行起来把自启指向自己）。

### 7.3 单实例与"再启动一次"

- 互斥体 `leigodpause`：第二个实例启动时发现已存在就退出；
- 但退出前，如果带 `--show`，它会通过**跨进程消息**请第一个实例显示主窗口：`shutdown.rs` 里的 message-only 窗口（类名 `LeigodPauseShutdownWnd`）收到 `WM_APP+1` 就调 `state.native_show_window()`。
- 为什么不直接对别人的窗口 `ShowWindow`？因为 winit 内部记着"窗口是隐藏的"，被外部强行显示后状态就对不上了——**再点 X 收不回托盘**（实测）。

---

## 8. 配置与数据存储

### 8.1 config.ini 字段（exe 同目录，兼容旧 Python 版字段）

| 字段 | 默认 | 说明 |
|---|---|---|
| `path` | 空 | 雷神客户端路径；留空时自动查注册表 |
| `uname` | 空 | 手机号（短信登录用） |
| `password` / `md5` | 空 / `0` | 旧版密码字段，**当前登录链路不使用**，仅为兼容保留 |
| `games` | `notepad` | 游戏规则：进程名（如 `GTA5`）或目录（如 `D:\Games\...\WARDOGS`），逗号分隔 |
| `update` | 1 | 进程扫描间隔（秒） |
| `looptime` | 30 | 旧版字段，已被 `grace` 取代 |
| `grace` | 180 | 游戏全部退出后等待多久再暂停（10~600 秒） |
| `http_port` | 18100 | 本地验证码接口端口 |
| `autostart` | 1 | 开机自启 |
| `auto_recover` | 0 | 检测到游戏启动时自动恢复加速 |
| `auto_steam` | 1 | Steam 库内运行的程序视为游戏 |
| `clip_watch` | 0 | 进登录页自动复制命令后，30 秒内识别剪贴板 token 并验证保存 |
| `account_token` | 空 | **登录态凭据**，登录后自动写入 |
| `smscode_key` / `sms_expiry` | 空 | 最近一次短信验证码的标识与有效期 |

> ⚠️ **这些值都是明文存储**，详见 README 的「数据与隐私」章节。

### 8.2 热加载

`monitor.rs` 每轮都比对 `config.ini` 的文件修改时间，变了就重新读取并刷新黑板（含 token）。所以**用记事本改配置、或程序自己写配置，都能立刻生效，不用重启**。

---

## 9. 对外接口（给自动化和短信转发 App 用）

### 9.1 HTTP（只监听本机 127.0.0.1）

| 方法 | 路径 | 作用 |
|---|---|---|
| POST | `/token/sms` | 触发下发短信验证码，返回 `smscode_key` |
| POST | `/token/code` | body `{"code":"867020"}`，用验证码换新 token 并保存 |
| GET | `/status` | token 是否有效、账号暂停状态、监控状态、宽限剩余 |
| GET | `/health` | 存活探测 |
| POST | `/show` | 显示主窗口（供脚本调起界面） |

```bash
curl -X POST http://127.0.0.1:18100/token/sms
curl -X POST http://127.0.0.1:18100/token/code -d "{\"code\":\"867020\"}"
curl http://127.0.0.1:18100/status
```

### 9.2 命名管道

`\\.\pipe\leigod-sms-code`，每行一条指令：`sms` = 触发发码；纯数字（≥4 位）= 提交验证码登录。

### 9.3 CLI

```
leigod-pause.exe info      查询账号/暂停状态（打印原始 JSON）
leigod-pause.exe pause     立即暂停计时
leigod-pause.exe resume    恢复计时
leigod-pause.exe sms       触发下发短信验证码
leigod-pause.exe code <验证码>
leigod-pause.exe status    本机 /status 的返回
leigod-pause.exe steam     列出已安装的 Steam 游戏（验证扫描）
leigod-pause.exe autostart [on|off|status]
```

---

## 10. 常见修改任务：从哪下手

| 想做的事 | 改哪里 |
|---|---|
| 换/加游戏识别方式（比如识别 Epic） | `monitor.rs::find_game()`，仿照 Steam 那一段加一个"库目录"集合 |
| 改宽限时长、扫描间隔 | 不用改代码，设置页或 config.ini 直接调 |
| 改状态机行为（比如宽限内弹提示） | `monitor.rs` 的 `loop` 与 `Mode` 枚举 |
| 加一个设置项 | 三处：`config.rs`（字段 + 读 + 写）、`gui.rs`（编辑缓冲字段 + 界面控件 + 保存时写回） |
| 加一个 HTTP 端点 | `code_api.rs::route()` 加一个 `(方法, 路径)` 分支 |
| 改通知文案 | 搜 `push_notify`，调用点分布在 `monitor.rs` / `actions.rs` / `code_api.rs` |
| 调雷神接口（换字段、加接口） | `api/mod.rs`，先对照 `API_NOTES.md` |
| 改界面配色 | `gui.rs` 的 `tone` 模块（深浅主题两套色值） |
| 改开机自启行为 | `config.rs::set_autostart / refresh_autostart_params` |

---

## 11. 构建、调试、测试

```bash
cargo build            # 调试构建（target/debug/leigod-pause.exe）
cargo test             # 单元测试（签名算法、Steam vdf 解析、剪贴板 token 提取）
cargo build --release  # 发布单 exe（内嵌图标，target/release/leigod-pause.exe）
```

调试要点：

- **日志**：exe 同目录 `leigod_rs.log`（UTF-8，用 VSCode 打开；GBK 终端 `type` 会乱码）。每次启动都有一行「启动模式：…；控制台：…」，能直接看出是静默还是带控制台。
- **看实时日志**：`leigod-pause.exe --console` 开一个控制台窗口跟着滚。
- **界面机械化验证脚本**（`scripts/`，纯 ASCII，避免 PowerShell 按 GBK 解码脚本出错）：
  - `check_windows.ps1 -TargetPid <pid>`：列出该进程所有顶层窗口及可见性，用来确认"静默启动"是真的没有可见窗口；
  - `hide_main_window.ps1 -TargetPid <pid>`：向主窗口发 `WM_CLOSE`（等价于点 X，应隐藏到托盘）；
  - `screenshot_window.ps1 -TargetPid <pid> -Out x.png`：用 `PrintWindow` 只截目标窗口（不会拍到桌面上其它内容）；
  - `test_shutdown_msg.ps1`：模拟关机消息 `WM_QUERYENDSESSION`，验证关机强制暂停。
- **日志里常见的行**（排查时对照）：
  - `启动检测：账号已处于暂停状态，无需处理` — 启动自动暂停的"已暂停就跳过"分支正常；
  - `暂停跳过：账号已处于暂停状态` — 关键节点去重生效；
  - `token 已更新，执行挂起的暂停请求` — 挂起补偿生效。

---

## 12. 术语表

| 词 | 意思 |
|---|---|
| **token / account_token** | 雷神账号的登录态字符串，接口鉴权全靠它，会过期 |
| **暂停计时 / pause** | 让加速器剩余时长不再流失（对应官方"暂停"按钮） |
| **恢复计时 / recover** | 重新开始消耗时长 |
| **宽限（grace）** | 游戏退出后等待这么久再暂停，给游戏重启留缓冲 |
| **挂起（pending_pause）** | 该暂停但没成功（token 失效/网络差），记下来稍后重试 |
| **AUMID** | Windows 通知要求的"应用身份"，决定通知标题显示成谁 |
| **message-only 窗口** | 只收消息、不显示的隐藏窗口，用来监听关机消息/剪贴板变化 |
| **GUI 子系统 / 控制台子系统** | exe 头里的一个标记，决定双击时系统给不给配一个控制台窗口 |
| **failover** | 主域名不通时自动换备用域名（`webapi.leigod.com` → `webapi.nn.com`） |

---

## 13. 合规与免责

本项目是**个人自用工具的非官方实现**，与雷神加速器官方无任何关系；所有接口行为来自对官方网页前端的观察与实测（记录见 `API_NOTES.md`）。
使用前请阅读 README 的「免责声明」，并自行评估账号与合规风险。