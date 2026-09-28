# 雷神加速器 API 现状调研（2026-09-27 实测）

> 供 Rust 重写参考。实测结论基于对 `webapi.leigod.com`、`webapi.nn.com` 的真实请求，
> 以及官网前端（www.leigod.com，Nuxt SPA，主 bundle `leigod_main.js`）的逆向分析。
>
> ⚠️ 本文档仅供个人学习研究与自用工具开发参考：这些接口**不是官方公开 API**，
> 随时可能变更、限流或失效；请勿用于商业用途、批量操作或绕过官方限制的场景。
> 使用风险与合规责任详见 [README 的免责声明](README.md#免责声明)。

## 〇、端到端验证已通过（2026-09-27 17:20）

用浏览器取得的有效 token 实测：
- `info` → code 0，`pause_status_id` 字段仍在（旧工程兼容）
- `recover` → code 0（恢复成功）
- `pause` → code 0（暂停成功）
- 新版 token 为约 31 位字符串（旧版 64 位，两种服务端都认）
- 注意：新版 info 响应里 `exp_time`/`mobile_num` 不一定出现，暂停判断只依赖 `pause_status_id`

## 一、总体结论

| 能力 | 状态 | 说明 |
|------|------|------|
| `/api/user/info`  查询账号/暂停状态 | ✅ 实测通过 | 需有效 token，旧 token 返回 400006 |
| `/api/user/pause` 暂停计时 | ✅ 实测通过 (code 0) | 旧 token 返回 400007（过期） |
| `/api/user/recover` 恢复计时 | ✅ 实测通过 (code 0) | 旧 Python 工程没有此功能 |
| `/api/auth/login/v1` 密码登录（老接口） | ❌ 已死 | CloudWAF 拦截，HTTP 418；带 WAF cookie 也无效 |
| `/api/auth/login/v2` 密码登录（新接口） | ⚠️ 需极验 | 必须带 Geetest 验证码，无法纯后台自动化 |
| `/tools/smscode` + `/api/auth/login/code` 短信登录 | ✅ 可用 | 需要用户手机收到的短信码，半自动 |
| `webapi.nn.com` | ✅ 镜像后端 | 同一套接口，可做备用域名 |

token 优先方案（同 hobk/leishen-auto）完全可行；重新获取 token 走短信登录。
token 获取方式：官网登录后 Console 执行
`JSON.parse(localStorage.getItem("account_token")).account_token`

## 二、通用约定

- API Host：`https://webapi.leigod.com`（备用 `https://webapi.nn.com`）
- Content-Type：新前端用 `application/json`；旧的 form-urlencoded 也兼容（实测）
- 请求头：正常浏览器 UA + `Origin`/`Referer: https://www.leigod.com/`（加指标）
- 通用 query 参数：`os_type=4&region_code=1&src_channel=guanwang&lang=zh_CN`
- 鉴权：`account_token` 放 query 或 body 均可（实测两种都被服务端接受）
- 签名算法（login/v2 用，**与旧版相同**，key 未变）：
  ```
  ts = floor(now/1000)
  str = "k1=v1&k2=v2&...&ts=...&key=5C5A639C20665313622F51E93E3F2783"
        （参数按 key 字典序排列，值不做 urlencode）
  sign = md5(str)
  ```
- 会话失效码（官网前端统一按登出处理）：`400006, 400007, 400008, 400816, 400334, 400027`
- 其他码：`0` 成功；`400803` 已是暂停状态；`400856` 极验校验失败；`400001` 参数缺失；`500001` 业务校验失败

## 三、核心接口

### 1. 查询账号信息（只读）
```
POST https://webapi.leigod.com/api/user/info
{"account_token": "<token>"}          // 或放 query
→ code 0: data.pause_status_id (0=加速中 1=已暂停), exp_time(剩余秒), mobile_num ...
→ code 400006: 账号未登录（token 失效）
```

### 2. 暂停计时
```
POST https://webapi.leigod.com/api/user/pause
{"account_token": "<token>"}
→ code 0 成功 / 400803 已暂停 / 400007 登录态过期
```

### 3. 恢复计时（新增）
```
POST https://webapi.leigod.com/api/user/recover
{"account_token": "<token>"}
```

### 4. 短信登录（推荐的重登路径）
```
① POST /tools/smscode                       ← 注意路径无 /api 前缀
   {"phone": "<手机号>", "country_code": 86, "state": 4}
   → data.smscode_key (30分钟有效), data.bind_status(5=正常绑定), data.has_password
   → 同时向该手机下发真实短信验证码
② POST /api/auth/login/code
   {"country_code": 86, "mobile_num": "<手机号>",
    "smscode_key": "<①返回>", "smscode": "<短信码>",
    "code": "", "password": "", "refer_code": ""}
   → data.login_info.account_token
```

### 5. 密码登录（需过极验，不建议自动化）
```
POST /api/auth/login/v2   (带 sign)
验证码为 Geetest v4 字段：lot_number / captcha_output / pass_token / gen_time
极验配置（v3 经典格式 gt/challenge）：
  POST /tools/captcha/geetest/config  {"type":"web"}
老接口 /api/auth/login/v1 已被 CloudWAF 418 拦截。
```

### 6. 其他已知接口（官网前端）
- `/api/auth/micro/qrcode`、`/api/micro/login/web`：微信扫码登录
- `/api/user/time/log`：时长使用记录
- `/tools/system_time`：服务器时间（注意实际路径在 /tools/ 下）
- `/api/user/switch`、`/api/user/switch/info`：切换设备/节点相关

## 四、旧 Python 工程（F:\...\legod-auto-pause, 6yy66yy/legod-auto-pause v2.2.3）现状

- 流程：WMI 轮询游戏进程 → 游戏退出 30s 后调 pause；token 失效自动重登
- config.ini 里的 token 已过期（2024 年），密码登录链路因 WAF 已死
- pause 重试逻辑只处理 400006，现在 pause 过期返回 400007，需要补
- 独立运行方式：legod.py 主循环 / TrayIcon.py 托盘 / test.py 关机消息时暂停

## 五、Rust 重写建议

- HTTP：reqwest（blocking 或 tokio）；JSON：serde_json；MD5：md5 crate
- 登录策略：token 持久化 + 失效(上述 6 个码)时提示重新登录（短信流程，终端内输入验证码即可全自动）
- 游戏进程检测：sysinfo crate 按 process name 匹配，替代 WMI
- 托盘：tray-icon crate；开机自启：注册表 Run 键
- 配置沿用 config.ini 结构（兼容迁移），token 改为过期自动走短信重登
- 备用域名 failover：webapi.leigod.com → webapi.nn.com
