# P4 迁移事务：2026-09-11 工作区证据

## 证据身份与边界

- 仓库基线：`5a9a721064f3db8211b56532169f2b81b6212bde`；验证包含未提交工作区改动，未执行 commit / push。
- 环境：macOS 27.0 / arm64；Rust 1.98.1、Node 24.14.0、pnpm 11.24.0；联调服务为 Go 1.26.1 / MySQL 8.4.11。
- 本轮 release **二进制编译**：`target/release/doing-desktop`，SHA-256 `65b124264f0c793c56b1a3ae8337d745776622e759fabebb03cb798a3beaf710`。
  未构建/签名/公证或安装新 DMG，不使用旧 bundle、历史截图或旧性能数据替代本轮原生验收。
- 原计划 SHA-256 仍为 `e2f96b1f9f07f7902c0131614a365198081c79184f8ccbf4d597680720101e2c`。
- 不读真实旧任务/UserDefaults/凭据，不启动旧 Swift 应用，不修改原 Go API，不执行正式用户切换。

## 执行结果

| 命令/检查 | 实际结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过；修正自动解引用/多平台 cfg 分支末尾 return 提示后零 warning |
| `cargo test --workspace --locked` | **189 项通过**：core 44 单测 + 11 集成，desktop 134；0 失败 |
| `pnpm run ci` | **87 项 Vitest**、TypeScript 和 Vite 构建通过；保留既有 React 建议级 warning、notification 静态/动态导入重复提示 |
| `cargo test --locked -p doing-desktop --lib regenerate_types_gen -- --ignored` | 精确运行生成入口 1 项；常规漂移守卫通过 |
| `cargo check -p doing-core --target x86_64-pc-windows-msvc --locked` | Windows 核心/文件替换分支编译通过；不是 Windows Tauri 壳或运行验收 |
| `./scripts/exercise-disk-full.sh` | 新建 1 MiB 小卷真实 ENOSPC；替换保存与新文件发布失败不损坏已有文件、不留下截断导出；卷和目录已清理 |
| `cargo build -p doing-desktop --release --locked` | macOS arm64 编译通过（2m38s）；没有启动新产物 |
| `scripts/exercise-go-mysql.py … --report docs/evidence/go-mysql-2026-09-11-p4.json` | 全新私有 datadir 两阶段实际联调、MySQL/Go/Rust 重启通过；原源摘要一致，六个记录 PID 随后确认均已退出，临时数据已清理 |
| `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py'` | 7 项离线启动器守卫通过 |
| 生成后的 `pnpm exec tsc -b` / `pnpm exec vitest run src/lib/contract.test.ts` | 类型构建及 7 项命令/类型契约守卫通过，不重复加算到 87 项 |
| `git diff --check` | 通过 |

Rust 常规总数不含磁盘满、类型生成、真实数据库入口和私有 kill helper。kill 矩阵父测试包含在 desktop 134 项内；20 个子进程不是额外 20 项常规测试。
Go/MySQL 完整脱敏报告：[`go-mysql-2026-09-11-p4.json`](go-mysql-2026-09-11-p4.json)。

## 复现后修复的回归

1. `pnpm exec tsc -b` 复现 App 壳丢失 `run` 解构；恢复后构建/交互通过，设置窗口新增恢复入口用例。
2. 旧恢复请求与导入竞争：导入现在明确撤销旧会话。测试断言 `sessionChanged`、需要重新登录、导入内容/修订/无归属不变、无迟到候选、内存与磁盘一致。
3. `wrong_id_migration_commands_preserve_history_state_and_session` 和 `terminal_migration_commands_preserve_history_state_and_session` 在修复前失败：磁盘哈希不变但撤销标题被清空、引擎代次增加。
   现在拒绝/终态重试只读；已初始化空文件也保留 redo 和当前租约。`error_after_durable_completion_still_installs_committed_files_before_unlocking` 防止反向回归：完成标记已落盘时，即使返回错误也安装已提交文件。
4. `native_display_names_need_not_match_tao_model_names` 复刻锁定依赖的真实命名差异，修复前应恢复的位置变成 None。
   现在按完整屏幕几何与 scale 唯一匹配，不依赖 localizedName；旧偏好坐标测试接通实际窗口所用的 selector 和 Tauri Position。
5. 另保留前一实现轮次的红绿回归：错误记录不得提前推进 durable Committed；取消后的重新登录要求不得被后续无效导入清掉。

## P4 专项覆盖

- 源 SHA-256/大小/mtime/Unix 身份、旧进程/空间预检、可选白名单偏好、逐字段备份/暂存核验、目标已初始化/后续被修改、未知日志、锁竞争。
- Preparing/Ready/Publishing/Committed 与 RollingBack/Cancelled/Rejected/KeptCurrent；启动先恢复再加载，未完成时保护写入和认证。
- **20 个真实 kill 点**：8 个 Preparing 检查点、7 个发布/完成检查点、完成标记写入前间隙、4 个撤销检查点。
  父进程等到私有子进程到达检查点后终止确切 owned child、wait，并在 Unix 断言 `signal() == SIGKILL`。
  随后由新协调器从磁盘恢复/撤销，不依赖子进程析构、内存标记或假装进程退出。
- 原生 NSDictionary 过滤/序列化直接用内存合成对象验证，不访问真实 UserDefaults 应用域。
- 浮窗位置覆盖 Retina、负坐标副屏、目标 scale、工作区、拔屏/重叠歧义、非法尺寸/数值；macOS 明确输出 logical position，避免当前屏 scale 重解释。
- 主面板与设置窗口的导入/恢复入口、是否覆盖偏好、二次确认、Esc/焦点、错误/备份路径提示。

## 仍未完成

完整目标仍为 P0–P6，见 [`migration-progress.md`](../migration-progress.md)。本轮不是正式切换或原生验收：

- 真实多屏/混合 DPI、实际 tray rect 锚定（当前 popover 仍为工作区 fallback）、NSPanel/Spaces、IME 与性能预算；Windows 11、macOS 14/Intel。
- 旧 45 项行为逐项映射和所有公开命令/副作用边界审计；迁移之外的崩溃矩阵。
- 系统 Keychain/Windows 凭据、真实旧用户域/登录项语义、通知跨重启定位与安装态点击。
- 双端同账号原生回归、正式 HTTPS、签名/公证、架构安装包、权限/日志/依赖审计、干净机器安装升级卸载及用户切换批准。
