# Activity

## 2026-09-12：sz / rz（ZMODEM）支持

### 环境

- worktree `/Users/nickhaoxu/my-stdio/agenterm-zmodem`，分支 `feat/zmodem-rzsz`，基线 `28cc180`。
- 主 worktree `/Users/nickhaoxu/my-stdio/agenterm` 有他人未提交改动，全程未触碰。
- mini（Apple M4）用于编译 `warp_terminal` 层；GUI（`-p warp`）因缺 macOS SDK bindgen 头文件只能本机编译。
- **坑**：两份 app 的 bundle ID 都是 `dev.agenterm.Agenterm`，`open` 会切到已运行的旧实例。验证时务必先
  `pkill -f "my-stdio/agenterm/target"`，再从 `agenterm-zmodem` 启动，并确认 File 菜单里有
  「Send Files to Remote (rz)…」——有它才是本分支的构建。

### 当前状态

- **`sz` 下载**：可用，用户已实测确认（文本文件）。二进制大文件在流控修复前未验证。
- **`rz` 上传**：协议链路已打通（ZRQINIT → ZRINIT → ZFILE → ZRPOS → 数据），`rz` 接受文件名与大小，
  但最后一次实测因 PTY 流控问题未能落盘。修复已提交，**尚未经用户实测验证**。
- 测试：39 单测 + 5 个真实 lrzsz 集成测试全绿，无 ignore。

### 实现要点

- `crates/warp_terminal/src/zmodem.rs`：wire 原语（CRC-16/XMODEM、CRC-32/ISO-HDLC、ZDLE 转义、
  ZHEX 头编解码）刻意与 `zmodem2` 一致；`ZmodemDetector` 要求完整合法头（含 CRC）才接管，普通二进制输出不受影响。
- 事件循环 `route_zmodem` 为自由函数，放在取 `TerminalModel` 锁**之后**（锁前会因 `continue` 重读导致同一批字节
  被重复喂进状态机）；失败一律回退渲染。
- 进度按 WeTERM 方式渲染进终端本身（`\r` + `\x1b[K` 原地刷新，100ms 节流），结束留摘要在 Block scrollback。
- `rz` 检测到 ZRINIT 自动弹文件选择框（WeTERM 模型）；File 菜单入口保留作为兜底。
- 20 秒无进展自动放弃传输并归还终端（此前 session 会吞掉所有 PTY 输出，Ctrl-C 也出不来）。

### 关键排查结论（都有字节级证据）

1. **`zmodem2` 的 ZFILE 与 lrzsz 不兼容**：缺元数据行和 ZCONV 标志。对照实验：用库自带 ZFILE 时 `rz` 直接
   TIMEOUT；用自建帧才会 `Receiving: <file>`。已自建 `zfile_frame()`，格式对照真实 `sz` 抓包逐字节验证。
2. **PTY 流控是最终的卡死根因**：`unix.rs` 只设了 `IUTF8`，`IXON` 一直开着。真实传输抓包显示同一个 10317 字节
   数据帧重发 21 次，夹杂 72 次单字节 `0x11`/`0x13`——二进制文件里的这两个字节被 line discipline 当 XON/XOFF
   吃掉了。已在传输期间关闭 `IXON`/`IXOFF` 并在结束/失速/中止时恢复。
3. **探测器曾漏掉逐字节到达的前导**：缓冲末尾的单个 ZPAD 被当普通字符丢弃。已修，并有逐字节投喂的回归测试。
4. **`submit_wire` 曾丢弃缓冲尾部**：receiver 有待处理工作时只消费一部分，而 ZEOF 正在那段尾巴里。已改为
   输入与 drain 交替。
5. **`ZmodemTransfer` 未注册为 singleton**：首个 ZMODEM 事件即 panic，表现为「卡死」。已在 `lib.rs` 注册。

### 我判断错过的地方（供接手者参考，不要重复）

- 曾三次反复认定「ZCRCW vs ZCRCE 是 `zmodem2` 的 bug」。做过对照实验：**把帧尾改回 ZCRCW，测试同样通过**，
  所以该假设未被证实。规范里 ZCRCW 也合法，只是多一次往返。真正修好测试的是 raw PTY（commit `487ef25`）。
- 曾据「日志里没有 ZMODEM 记录」断定探测未触发，但当时探测成功路径上根本没有日志，该推断无依据。
- 曾断言「真实终端本来就是 raw 模式」——错的，见上面第 2 条。

### 诊断工具

- `WARP_ZMODEM_TRACE=<path>` 记录原始 PTY 字节、协议收发、上传请求，默认关闭。
  格式：`方向 字节数 十六进制`，方向为 `pty` / `upload-request` / `upload-start` / `in` / `out` / `detect`。
- 复现素材：`/tmp/zm_e2e/`（`to_upload.txt` md5 `4133c27d103537d059f6ed906867b6c1`、
  `big_upload.bin` md5 `f639b86334ec0261b23f157b5a7bb3a8`、收件目录 `inbox/`）。
- 抓真实 `sz` 帧的一次性 harness 在 `/tmp/zm/harness`（非仓库内容，可能已被清理）。

### 下一步

1. 用带流控修复的构建实测 `rz` 上传二进制文件（xlsx 那个用例），确认数据帧只发一次并落盘。
2. 同样用二进制文件复测 `sz` 下载——此前只验过文本，可能同样受流控影响。
3. Command Palette 在 Agenterm 里整体不可用（`97f6512` 删菜单时连带失效），可能影响其他功能，未处理。
4. 未做：传输进度面板/取消按钮（当前只有终端内进度行与 toast）、Windows 平台的流控处理
   （`set_flow_control` 在非 unix 为 no-op）。
