# GitHub Actions CI 首轮验证记录 · 2026-09-14

范围：计划 §11.1「Rust 核心和前端检查在 CI 运行；安装包分别在 macOS / Windows runner 构建，不假定一台 Mac 能完成所有正式包的构建和验收」的首次落地。
本轮只新增 CI 与文档，不改业务代码；没有停止本地开发、没有安装或运行 runner 产物、没有触碰签名/公证与发布通道。

| 项目 | 值 |
| --- | --- |
| 工作流 | [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)，`a347cee` 首次入库 |
| run #1 | [`34794672684`](https://github.com/zxcodenb/doing-tauri/actions/runs/34794672684) @ `a347cee`，5/5 成功 |
| run #2 | [`34795422690`](https://github.com/zxcodenb/doing-tauri/actions/runs/34795422690) @ `75d080e`，5/5 成功 |
| 触发 | `push`(`main`) / `pull_request` / `workflow_dispatch`；`permissions: contents: read`，无任何 secret |
| 环境 | runner 自带 `macos-latest`(arm64) / `windows-latest`(x64) / `ubuntu-latest`；Node 24、pnpm 11、Rust stable（[`rust-toolchain.toml`](../../rust-toolchain.toml)）、`--locked` |

本机环境（对照）：macOS 27.0 arm64、Node 24.14.0、pnpm 11.24.0、Rust 1.98.1。CI 未设置 `DOING_API_URL` 等发布环境变量。

## 1. 结果

run #2（缓存命中，作为稳态耗时；run #1 冷启动为前端/核心 <1m、应用层 3m、macOS 7m、Windows 10m）：

| Job | Runner | 耗时 | 结果 | 覆盖 |
| --- | --- | --- | --- | --- |
| 前端门禁 | ubuntu | <1m | 通过 | `pnpm lint`、**102 项 Vitest / 8 文件**、`tsc -b && vite build` |
| Rust 核心门禁 | ubuntu | <1m | 通过 | `cargo test -p doing-core --locked`：44 单测 + 11 集成；`clippy --all-targets -- -D warnings` |
| 应用层门禁 | macos | 1m | 通过 | 先 `pnpm build`（`generate_context!` 编译期嵌入 `frontendDist`），再 `cargo test --workspace --locked` 与全 workspace clippy |
| build-macos | macos | 3m | 通过 | `pnpm tauri:build` → DMG + SHA-256 |
| build-windows | windows | 6m | 通过 | `pnpm tauri:build`（自动合并 `tauri.windows.conf.json`）→ NSIS + SHA-256 |

测试口径与本地一致：**206 项通过**（core 55 = 44 + 11，desktop 144，doing-notifications 7），**5 项 ignored**（core 1 项 MySQL 受控入口；desktop 4 项类型生成/磁盘满/私有 kill helper 等受控入口），不重复加算到 206。

## 2. 产物与校验

run #2（`75d080e`）：

| 产物 artifact | 文件 | 大小 (bytes) | SHA-256 |
| --- | --- | --- | --- |
| `doing-macos-dmg` | `Doing_0.1.0_aarch64.dmg` | 3,410,950 | `9636f63867ac67832abf2763222019a9ff643315e8bd5c3dd26ad8293a6c335f` |
| `doing-windows-nsis` | `Doing_0.1.0_x64-setup.exe` | 3,016,115 | `295fb8c1439a97269b3f9b5618d06a724ab2f6af500007b93ca4167c10c56405` |

run #1（`a347cee`）：DMG 3,397,662 bytes / `e559505f…d1fc3a`，NSIS 2,999,320 bytes / `15ae2915…db87f29`。

下载后在 macOS 本机复核（`gh run download` 两条 artifact）：

- `shasum -a 256 -c SHA256SUMS.txt`：DMG 与 NSIS 均 **OK**（run #2）；
- `hdiutil imageinfo Doing_0.1.0_aarch64.dmg`：UDIF zlib 只读镜像，CRC32 有效，非损坏文件；
- **同一 commit 两次 run 的产物哈希不同**（时间戳/路径进入打包），本项目不宣称可复现构建；完整性以各自 artifact 内 `SHA256SUMS.txt` 为准；
- artifact 保留 30 天，过期后可从对应 run 重新构建/下载。

## 3. 本轮修复的两个 CI 缺陷

| 缺陷 | 红灯证据 | 修复与绿灯 |
| --- | --- | --- |
| PowerShell `Out-File` 写 CRLF 校验文件 | run #1 的 Windows `SHA256SUMS.txt` 含 `\r`，macOS 上 `shasum -c` 报 `No such file or directory`（剥掉 `\r` 后哈希本身 OK） | 改用 `[System.IO.File]::WriteAllText` 写 LF；run #2 `od -c` 确认行尾仅 `\n`，`shasum -c` 直接 OK |
| Node.js 20 弃用警告 | run #1 日志列出 `checkout@v4 / setup-node@v4 / upload-artifact@v4 / pnpm/action-setup@v4` 被强制跑在 Node 24 | 升到 `checkout@v7 / setup-node@v7 / upload-artifact@v7 / pnpm/action-setup@v6`（输入参数逐个核对）；run #2 同 job 日志命中 `node.js 20 is deprecated` **0** 次 |

## 4. 尚不能据此宣布完成

- **产物未签名、未公证**：仅供内部安装测试，不等于 D07 的 macOS Developer ID / Windows 签名可发布产物；也没有版本策略、自动更新或商店分发。
- **CI runner 不能替代实机验收**（计划 §11.1）：托盘真实鼠标点击、IME 候选窗、通知横幅/声音/安装态热冷点击、多屏与 Spaces、旧用户域/登录项、干净机安装升级卸载，均未在 CI 中执行。
- **Windows 缺口只补到构建层**：原生 Windows runner 上完整桌面壳编译 + NSIS 打包通过，解除了 2026-09-11 记录的 macOS 交叉编译 ring/Windows SDK `assert.h` 阻塞；Windows 运行验收仍缺，`Windows 完整桌面 cross-check 未通过` 的历史结论在其运行层面继续有效。
- **最低 macOS 版本与 Intel 架构**：`macos-latest` 仅产出 arm64；未产出 universal/Intel 包，也无 Intel 实机证据。
- **真实服务与凭据**：CI 未接触生产 DB、Token、Keychain 或旧用户数据；Go/MySQL 受控入口与迁移/磁盘满私有入口仍按受控方式另行执行。
- 附件真机 go/mysql 联调、P5/P6 门槛与旧 Swift 客户端退役条件不受本轮影响，P0–P6 全部目标仍未完成。
