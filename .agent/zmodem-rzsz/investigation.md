# ZMODEM 接手调查

日期：2026-09-12。调查基线 `9fd3f9f`；产品代码未修改。

## 结论与证据等级

保留在 ANSI parser 前接管 PTY 的边界；重做会话生命周期、文件 I/O 和事件契约。
现有问题不能仅归因于 termios；协议依赖是否保留，须经过扩展互通验证决定。

下列“代码事实”来自当前源码，能证明对应条件下的行为，但不冒充用户历史失败的现场复现。
旧 activity 中的抓包、用户实测和临时 harness 在本轮未回放。

## WeTERM 参照

- 仓库：`/Users/nickhaoxu/my-studio/WeTERM`，`weterm` 分支；tracked 工作区干净。
- `xweterm` 固定提交 `222aa8b7875a9426808c163421745f425bcd90f0`，本地未初始化。
  尝试 `git submodule update --init --depth 1 xweterm`，远端两次返回 Git repository not found。
  没有改 WeTERM 源码；Git 为这次尝试注册了 submodule 的本地配置。
- 替代参照：`/Applications/WeTERM.app/Contents/Resources/app.asar`，本机版本 3.5.7，
  product commit `f950a4b4ae7ec5826ecf41fbd834860f3292d1df`。
  使用 `@electron/asar` 的只读 listPackage/extractFile API 读取发布代码，不修改安装包。
  这不是目标子模块提交的源码，存在版本差异；后续不宣称与缺失源码逐行一致。
- 以下相对路径均位于 asar 的 `node_modules/@tencent/theia-weterm-ssh/lib/`。

| 行为 | WeTERM 代码依据 | 当前 Agenterm |
| --- | --- | --- |
| 每终端隔离 | `node/zmodem/zmodem-session-manager.js` 的 sessions Map，terminalId 贯穿操作 | PTY session 独立，但 GUI pending download 为全局单例 |
| rz 自动多选 | `browser/zmodem/weterm-zmodem.js:231`，无 pending files 时选文件 | 已接入自动选择；无等待状态/去重/取消回写 |
| sz 接收前选目录 | 同文件 `:181`、`:267`、`:280` | 收完整文件后逐文件 Save dialog |
| 固定目录/记住目录 | 同文件 `:282`、`:297`、`:322` | 只有内存字段，且保存的仍是旧目录 |
| 重名策略 | `node/zmodem/zmodem-download-handler.js` 的 resolveFilePath | 无策略，只 File::create |
| 流式文件 I/O | download handler 的 createWriteStream/on_input；upload handler 的 8 KiB readStream | 双向整文件缓存，下载再 clone |
| 多文件可见状态 | upload handler 的 transfers Map 和 frontend 的 initUploadFileTransfers | 上传 bytes_sent 没有用于 UI 进度 |
| 拖拽 ZMODEM | frontend 的 pendingUploadFiles；`browser/preferences/weterm-preference.js:194` 的 rz 命令 | 拖拽仍走 SFTP/粘贴路径 |
| 取消 | frontend cancelReceiveZmodem -> backend cancelTransfer/endSession | 文件选择取消只 log；没有 cancel 消息 |
| 跨终端转发 | manager configureCrossTransfer/cancelCrossTransferPeer；frontend laboratory.feature.crossfit | 未实现；WeTERM 中为实验室可选功能 |

WeTERM 设置：`terminal.integrated.ssh.drag.zmodem.cmd` 默认 `rz -E`；下载重名策略
`newfile/normal/overwrite`，默认 normal；下载目录默认 Downloads；selectpath 默认 true。
这些是参照行为，不表示 Agenterm 必须照搬强制覆盖默认值或内部设置名称。

WeTERM 也不是无缺陷的协议 oracle：其代码存在 CRC/背压规避选项，并明确对部分 ZRPOS 错误
提示不支持续传；本任务对齐功能，不复制潜在路径安全或错误处理缺陷。

## 当前实现的问题

### 1. 全局下载缓冲缺少终端/传输身份

代码事实：`app/src/terminal/zmodem_transfer.rs:33` 只有一个 pending，`:44` 为 SingletonEntity；
`app/src/terminal/view.rs:13080` 起所有终端事件都操作同一个对象，传入参数没有 PTY/transfer ID。
不同名字会替换缓冲；同名文件会 append 到同一缓冲；任何终端 Finished 都会清空 pending。
这直接导致并发传输串数据/丢数据风险，上传完成也可能清掉其他终端的下载。

### 2. 文件 I/O 与背压契约不成立

代码事实：`crates/warp_terminal/src/zmodem.rs:924` 的 offer_next_file 调用 std::fs::read 整文件。
从 route_zmodem 进入时，`local_tty/event_loop.rs:598` 起已经持有 TerminalModel 锁。
`app/src/terminal/zmodem_transfer.rs:102` clone 全量下载，`:104` 的 GUI 回调同步 File::create/write_all。
大文件/慢盘会放大内存和卡顿；磁盘出错时远端协议可能早已认为成功。
Receiver 的 file_written 本意是确认持久化，但当前 drain 只把数据拷进 Vec 就立即确认。

### 3. 取消不完整，中止序列错误

代码事实：`view.rs:26465` 的 picker cancel/error 只写日志，不取消对端；Message 没有取消传输命令。
`event_loop.rs:515` 把所有 Input 直接写入普通 write_list，Ctrl-C 不改变本地 transfer 状态。
`zmodem.rs:540` abort_sequence 只发送 2 CAN；其单测只验证这一错误形状。
zmodem2 包附带的 `docs/zmodem.txt:807` 要求至少 5 个连续 CAN；`:1277` 推荐 8 CAN + 10 BS。
WeTERM 内的 `node_modules/zmodem.js/src/zmlib.js:39` 使用 5 CAN。

### 4. 静默超时没有被调度

代码事实：20 秒 deadline 只在 `event_loop.rs:241` 收到新 bytes 后判断。
真正的 poll 在 `:830` 只用 ANSI synchronized-output timeout，不包含 transfer deadline。
对端静默且没有同步输出时没有 timer 唤醒。协议适配器也没有调用 Sender/Receiver::timeout。
另外“有任何 to_pty 就重置 last_progress”不能区分有效进展和无限握手/重试。

### 5. 等待文件选择不是一个状态

代码事实：每次合法 ZRINIT 都在 `event_loop.rs:305` 发 UploadRequested，reset detector 后返回。
此时 active 仍为空，重发握手会再次请求对话框；原握手和同 read 的后续字节直接丢弃。
picker callback 没有会话代次，无法判断用户选择是否属于已结束的等待请求。

### 6. 输出契约丢失顺序和未消费字节

代码事实：`zmodem.rs:670` 用一个 file_data 和独立 events Vec 汇总一次驱动的全部动作，
丢掉 data 与 FileStarted/FileCompleted 的相对顺序；`:765` 把 WriteFile 都拼进同一个 chunk，offset 固定 0。
`event_loop.rs:407` 先处理所有 data，`:417` 再处理 events，多文件/合并输入时存在归属风险。
适配器没有向调用者返回 consumed/remainder，完成后也继续喂本次剩余 input；
无法可靠把同 read 中的 shell prompt 交还 ANSI。具体分片组合待回归测试固定。

### 7. 普通终端输出也会被影响

代码事实：detector 遇到尾部 `*`/`**` 等候选前缀会保留，但没有 flush/deadline/EOF 接口。
没有下一批输出时，普通 `printf '*'` 的末尾可永久不显示。
合法但不是会话起始帧时 `event_loop.rs:313` 只返回 render_before，丢弃 protocol 和同 read 尾部。
所有 local PTY 默认扫描，未提供设置/capability gate。

### 8. termios 的恢复不是恢复原值

代码事实：`local_tty/unix.rs:694` 结束时强制打开 IXON/IXOFF，不保存开始前的状态。
若 IXON 已符合目标状态直接 return，IXOFF 可以仍不符合目标状态。
在 SSH 子进程自身管理 raw/cooked 设置时，贸然打开 IXON/IXOFF 会改变既有终端行为。
库的 ZDLE_TABLE 已转义数据中的 0x11/0x13；wire 中裸 XON/XOFF 也可能是合法流控。
因此旧 activity 的“看到 0x11/0x13，所以数据被吞”不够证明历史根因，应重新分层抓取证据。

### 9. 完成和进度语义不可信

代码事实：上传累积 bytes_sent，但 event loop 只把它用于判断 activity，不更新上传进度。
zmodem2::Sender 不发 FileStarted，GUI 却依靠该事件提供名字与总大小。
Sender 将 ZSKIP 映射成 FileCompleted，现有适配层不区分跳过与实际传输成功。
保存对话框尚未完成时，SessionCompleted 已可能触发“transfer complete” toast。
last_directory 保存的是展示对话框前的目录，不是用户最终选择的目录。

### 10. 诊断功能泄露内容且影响热路径

代码事实：`event_loop.rs:200` 的 opt-in trace 每批 open/append/hex，并在 `:291` 记录非传输的全部 PTY 输出。
可能包含凭据、命令结果、文件内容，且在持锁处理路径产生同步 I/O。
需改成有身份标识、限量、默认元数据的诊断；原始内容仅在明确受控测试时使用。

## 协议选型判断

- zmodem2 0.7.2 的 Action 已支持异步式 ReadFile/WriteFile；并不要求整文件内存常驻。
- Receiver 已有 set_manual_file_accept、accept_file_at、skip_file，可先做目标文件策略再接收。
- Sender/Receiver 有 timeout API，但当前封装未使用。
- 库的 ZFILE 仅发送 name/size，当前适配层替换整帧；此兼容补丁需要独立对照实验，
  特别是首次/重复 ZFILE、协商能力、二进制标志、ZSKIP 和远端正常退出。
- 不在本轮凭库内自测或已有 raw-PTY 绿灯决定“保留/淘汰”；优先小而可维护的 native 方案。
- 不先引入 zmodem.js 运行时或 bundled lrzsz。前者增加运行时/打包面，后者需要分发和许可证评估。

## 本轮验证

| 命令 | 结果 |
| --- | --- |
| `cargo nextest run -p warp_terminal --features local_tty -E 'test(zmodem)' --no-fail-fast` | 39 passed；过滤器未选中 lrzsz binary 内的测试 |
| `cargo nextest run -p warp_terminal --features local_tty --test zmodem_lrzsz --no-fail-fast` | 5 passed，0 skipped；本机 sz/rz 存在 |
| `cargo nextest run -p warp_terminal --features local_tty --no-fail-fast` | 594 passed，2 skipped；包含上述 44 项 |
| `cargo clippy -p warp_terminal --features local_tty --all-targets --tests -- -D warnings` | passed |
| `./script/format --check` | passed |

以上结果不包含 app GUI 单测、完整 workspace Clippy、GUI 构建/交互、本机 SSH 或现网验收。

现有 integration test 绕过 detector、route_zmodem、ChannelEventListener、GUI 下载模型和真实输入路径。
下载测试在结束或 idle 后 kill 对端，不要求对端正常退出；测试的部分写入循环没有总超时，
上传 child.wait 也没有硬 deadline。后续 harness 需要总时限和子进程清理，成功必须要求完整关闭。

## 下一步

先审批修订后的 plan，然后补齐 spec 和 task 契约。优先用完整链路回归固定上述问题；
每个库问题必须有与适配器问题分离的重现，不延续未复现的历史诊断。
