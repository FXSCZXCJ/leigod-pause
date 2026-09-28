# legod-pause — 雷神加速器时长自动暂停 (Rust 版 v3.0)

原 Python 项目 [6yy66yy/legod-auto-pause](https://github.com/6yy66yy/legod-auto-pause) 的 Rust 重写版。
API 现状调研与实测结论见 [API_NOTES.md](API_NOTES.md)。

## 功能

- **启动即静默**：编译为 GUI 子系统程序，双击 / 开机自启都不弹控制台、不弹界面，直接进托盘
  - `legod-pause.exe`：后台静默运行（默认，开机自启入口就写这个参数 `--hidden`）
  - `legod-pause.exe --show`：启动时打开主界面；若程序已在运行则把主界面叫到前台
  - `legod-pause.exe --console`：额外开一个控制台窗口实时看日志（调试用）
- **托盘常驻**：右键菜单（打开主界面 / 暂停时长 / 恢复时长 / 打开雷神 / 退出并暂停 / 退出），tooltip 实时显示监控状态，状态变化弹 Windows 通知
- **主界面**（egui 四页签）：
  - 状态：token 灯 / 账号暂停状态 / 监控状态与宽限倒计时 / 手动暂停·恢复·查询按钮
  - 设置：游戏进程名列表、宽限时长(10-600s)、轮询间隔、雷神路径、开机自启、自动恢复加速开关
  - 登录：发送短信验证码 → 输码 → 一键更新 token；也支持直接粘贴浏览器提取的 token
  - 日志：实时滚动
- **登录页取 token**：命令会**在进入登录页时自动复制到剪贴板**（也可用「📋 复制命令」按钮再复制一次）；
  开启设置里的「剪贴板自动识别 token」后，进入登录页同时开启 30 秒监听
  （系统剪贴板变更通知，不轮询、不占用剪贴板），识别到 token 先调接口验证、
  验证通过才写入配置，同时保留手动粘贴输入框
- **游戏监控**：轮询进程列表，游戏全部退出后进入 **180 秒宽限**（可调），期间游戏再次启动立即取消暂停，倒计时结束自动调用暂停接口
- **关机强制暂停**：钩住 WM_QUERYENDSESSION/WM_ENDSESSION，关机前同步调暂停接口（2s 超时 × 2 次重试）
- **token 自动续期接口**（短信验证码双通道，供短信转发 App / 脚本全自动续 token）：
  - HTTP：`POST http://127.0.0.1:18100/token/sms`（触发发码）→ `POST /token/code`，body `{"code":"867020"}`（换新 token）
  - 命名管道：`\\.\pipe\legod-sms-code`，每行一条，`sms`=触发发码，纯数字=提交验证码
  - `GET /status` 实时状态、`GET /health` 存活探测
- **CLI**：`legod-pause.exe info|pause|resume|sms|code <验证码>|status|help`
  （在终端里运行会自动接上父控制台，输出照常显示；`| more`、`> out.txt` 重定向也可用）
- token 失效时暂停请求**挂起**，任意通道更新 token 后自动补暂停

## 配置

exe 同目录 `config.ini`，**完全兼容旧版字段**（可直接拷贝旧文件的 games/uname/password 等）：

```ini
[config]
path = "C:\Program Files (x86)\LeiGod_Acc\leigod.exe"  ; 雷神客户端路径
uname = 13800000000          ; 手机号
games = GTA5,Overwatch,notepad
update = 1                   ; 轮询间隔(秒)
grace = 180                  ; 游戏退出后宽限秒数(新增)
http_port = 18100            ; 验证码接口端口(新增)
autostart = 1                ; 开机自启(新增)
auto_recover = 0             ; 检测到游戏启动自动恢复加速(新增,默认关)
auto_steam = 1               ; Steam 库内程序视为游戏(默认开)
clip_watch = 0               ; 剪贴板自动识别 token(默认关)
account_token = ...          ; 登录后自动写入
```

> 登录说明：雷神 2024 年后给密码登录加了 CloudWAF 拦截 + 极验验证，纯密码 API 登录已不可行；
> 本工具用 **token 鉴权 + 短信验证码重登** 方案（实测可用，详见 API_NOTES.md）。

## 开发

```bash
cargo build          # 调试构建
cargo test           # 单元测试（签名算法）
cargo build --release # 发布单 exe（内嵌图标）
```

测试脚本：`scripts/test_shutdown_msg.ps1`（向运行中的程序发送 WM_QUERYENDSESSION 模拟关机）。
日志文件 `legod_rs.log`（UTF-8，建议用 VSCode 等查看，GBK 终端 type 会乱码）。
