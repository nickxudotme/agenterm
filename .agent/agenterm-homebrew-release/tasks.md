# 代码实现任务清单

> 基于 plan.md 生成
> 任务总数：5
> 状态标记：[ ] 待办, [~] 进行中, [x] 完成, [-] 跳过/废弃, [!] 阻塞

## 当前状态

- 当前任务：全部完成
- 阻塞任务：无
- 下一个可执行任务：无
- 上次同步：2026-09-13，完成公开发布、tap 更新与在线 Homebrew 安装验证

## 依赖关系与并行化

- 顺序执行：任务 1 → 任务 2
- 并行组 A（任务 1 完成后）：任务 2、任务 3
- 顺序执行：任务 4（依赖任务 2、3）
- 顺序执行：任务 5（依赖任务 4）

### 并行组

- 任务 2 与任务 3 可并行实现；workflow 使用任务 1 定义的 artifact 契约，cask 使用同一命名契约。
- 任务 4、5 必须串行，避免用未经校验的 artifact 或 cask 进入 review/release。

## 任务列表

### 任务 1：[x] 固化 macOS release artifact 与版本元数据
- **文件**：`app/Cargo.toml`、`script/macos/run`、`script/update_plist`、新增 release 打包脚本、`.gitignore`
- **依赖**：无
- **来源映射**：plan 成功标准 1、5、6
- **说明**：把现有本地 bundle 能力整理为 CI 可调用、可验证、输出稳定命名 zip/SHA 的发布入口，
  设置 Bundle ID `me.nickxu.agenterm`，并忽略本地 `dist/`。
- **context**：
  - `script/macos/run`：现有 app bundle、资源准备与 ad-hoc 签名流程
  - `script/update_plist`：tag 到 plist 版本的转换
  - `app/Cargo.toml [package.metadata.bundle.bin.agenterm]`：bundle 元数据
  - ScreenOff `scripts/build-release.sh`：稳定 artifact 生成模式
- **验收标准**：
  - [x] 本地脚本生成 `Agenterm-<version>-arm64.zip` 与 SHA-256
  - [x] app 可执行文件、架构、Bundle ID、版本、图标与 ad-hoc 签名检查通过

### 任务 2：[x] 增加 GitHub Release workflow
- **文件**：`.github/workflows/release.yml`、发布说明文档
- **依赖**：任务 1
- **来源映射**：plan 成功标准 1、2、6、7
- **说明**：复用 ScreenOff 的 tag/manual workflow，构建并发布 zip，随后以 deploy key 更新 tap；
  secret 缺失时保留 release 并输出明确 notice。
- **context**：
  - 任务 1 的 release 脚本与 artifact 命名
  - ScreenOff `.github/workflows/release.yml`、`docs/release.md`
  - `nickxudotme/homebrew-tap/Casks/screenoff.rb`
- **验收标准**：
  - [x] workflow YAML 可解析，tag/manual 版本校验明确
  - [x] release、SHA 与 tap 更新步骤引用同一 artifact
  - [x] 不在仓库中存储 deploy 私钥

### 任务 3：[x] 在个人 tap 增加 Agenterm cask
- **文件**：`nickxudotme/homebrew-tap/Casks/agenterm.rb`、tap README（按需）
- **依赖**：任务 1
- **来源映射**：plan 成功标准 2、3、4、5
- **说明**：新增 Apple Silicon cask，安装 `Agenterm.app`，homepage 指向个人域名，并提供
  ScreenOff 风格的中英文 caveats。
- **context**：
  - `Casks/screenoff.rb`：tap 约定与 caveats 风格
  - 任务 1 的 URL、版本和 artifact 命名
- **验收标准**：
  - [x] Ruby 语法与 `brew style` 检查通过；在线 audit 留待 release URL 可用后执行
  - [x] 安装路径、依赖、homepage、quarantine 命令正确

### 任务 4：[x] 本地端到端构建与 Homebrew 安装演练
- **文件**：任务 1-3 产物，不新增产品逻辑
- **依赖**：任务 2、任务 3
- **来源映射**：plan 成功标准 3、5、6
- **说明**：用本地构建 artifact 临时替换 cask URL/SHA 做安装、bundle 校验、启动与卸载演练，
  不创建公开 release。
- **context**：
  - release zip 与 SHA
  - Homebrew tap/cask 测试命令
- **验收标准**：
  - [x] `brew install --cask` 安装成功并显示 caveats
  - [x] 临时 appdir 中的 `Agenterm.app` 可验证，卸载成功

### 任务 5：[x] 评审并准备正式发布
- **文件**：两个仓库最终 diff 与 task artifacts
- **依赖**：任务 4
- **来源映射**：plan 全部成功标准与 Gate: Code Review、Release
- **说明**：运行独立 code review，关闭成立问题，准备两个仓库提交、deploy key 配置与 tag，
  在推送正式 release 前停在 Release Gate。
- **context**：
  - Agenterm 与 homebrew-tap 最终 diff
  - workflow 与本地安装验证输出
- **验收标准**：
  - [x] Code Review 通过或问题均关闭
  - [x] 发布 commit、版本、artifact 和 secret 状态均已验证

## 来源覆盖映射

| 来源 | 任务 | 说明 |
|---|---|---|
| 版本化 app、zip、SHA、Bundle ID | 1、2 | 构建契约与 CI 发布 |
| 自动更新 tap、secret 缺失降级 | 2 | workflow |
| cask、homepage、中英文 caveats | 3 | tap |
| 实际安装、启动文件检查、卸载 | 4 | 本地端到端验收 |
| 静态检查、review、发布证据 | 1、2、3、5 | 质量与交付 |
