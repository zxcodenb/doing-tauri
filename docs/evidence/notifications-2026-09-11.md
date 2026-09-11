# 通知重构验证记录 · 2026-09-11

范围：原计划 §6.2 的原生通知、持久点击定位与 UI 确认，以及重构后的全工作区回归。
基线：`5a9a721064f3db8211b56532169f2b81b6212bde` 后的未提交工作区；没有 commit/push、安装新产物或 P6 切换。
原计划 SHA-256：`e2f96b1f9f07f7902c0131614a365198081c79184f8ccbf4d597680720101e2c`。
旧 P4 历史记录 [`p4-migration-2026-09-11.md`](p4-migration-2026-09-11.md) 不作覆盖式改写。

环境：macOS 27.0 arm64，Rust 1.98.1、Node 24.14.0、pnpm 11.24.0、Go 1.26.1、MySQL 8.4.11。
这些是本机开发版本，不证明最低系统、Intel 或 Windows 真机兼容性。

## 1. 改动与证据边界

- 新 `doing-notifications` crate 提供 macOS UNUserNotificationCenter 和 Windows WinRT 薄适配器。
  实际权限查询、OS 接受结果、opaque UUID；macOS bundle guard 与原生异常边界，Windows XML 转义、Tag/Group 与安装协议。
- `notification-routes.json` 先预留后投递，原生点击同步落盘 Pending，main-only 拉取/ACK，每次检查有效会话、数据归属及迁移门禁。
- 前端握手包含 window-shown；任务行/焦点卡实际挂载并滚动后 ACK，旧会话迟到结果不回退当前 UI。
- Rust/JS 旧通知插件与 `system_notify_clicked` 移除，不再依赖旧 Granted/异步 show/数字 map。
- 实现协议和未完成的原生检查表见 [`../notification-delivery.md`](../notification-delivery.md)。

## 2. 实际执行结果

| 命令 | 本轮结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，零 Rust warning |
| `cargo test --workspace --locked` | **206 项通过**：core 44 单测 + 11 集成，desktop 144，native notifications 7；0 失败 |
| `pnpm run ci` | **102 项 Vitest**（8 文件）、TypeScript、Vite 通过；8 条 React 建议级 warning；旧 notification 导入重复提示已移除 |
| `cargo test --locked -p doing-desktop --lib regenerate_types_gen -- --ignored` | 1 项生成入口通过；再生成文件 SHA-256 不变 |
| `cargo test --locked -p doing-desktop --lib types_gen_is_up_to_date` | 漂移守卫通过，不另加算到 206 |
| `cargo check --locked -p doing-core -p doing-notifications --target x86_64-pc-windows-msvc` | 核心/Windows 文件替换与原生通知 crate 编译通过，不代表完整桌面壳 |
| `cargo check --locked -p doing-desktop --target x86_64-pc-windows-msvc` | **未通过，exit 101**：ring 0.17.14 C 编译找不到 Windows SDK `assert.h`；未造头文件、禁用 TLS 或缩目标冒充成功 |
| `./scripts/exercise-disk-full.sh` | 自建 1 MiB 卷实际 ENOSPC；替换保存与新建导出失败保持原文件、不残留截断导出；卷和目录清理后另行确认目录不存在 |
| `scripts/exercise-go-mysql.py … --report docs/evidence/go-mysql-2026-09-11-notifications.json` | 全新私有 MySQL 两阶段原 Go 路由/Rust 协调器通过；真实重启 Go/MySQL/Rust，未接触生产 DB |
| `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py'` | 7 项离线启动器安全守卫通过 |
| `cargo build --locked -p doing-desktop --release` | 本轮 macOS arm64 release 桌面二进制编译通过（4m25s）；未启动、打包安装或进行原生验收 |
| `git diff --check` | 通过 |

本轮产物：`target/release/doing-desktop`，8,468,000 bytes，SHA-256：
`4d52c081b3ec5e09f6cccdc9587307097657433c13f143d59ea02967ae385209`。
前端来自本轮 Vite `index-bfurZhJY.js` / `index-GAETks2S.css`；没有启动该产物，旧 bundle/DMG 不代表本轮已打包。

普通测试数不包含磁盘满、类型生成、真实数据库入口和两个私有 kill helper。
迁移/通知的两个父测试包含在 desktop 144 内；20 + 3 个真实 child 不是额外 23 项常规测试。
Go/MySQL 每阶段运行受保护 Rust 入口一次，不再叠加到 206。

## 3. 红绿回归与终止检查点

| 问题 / 复现入口 | 红灯与修复后的信号 |
| --- | --- |
| `notification_target_survives_restarting_the_app_with_the_same_account` | 旧内存 map 在新建 AppState 后丢失目标；现在从文件恢复并按当前账号定位 |
| `useNotificationNavigation.test.tsx` 的旧会话 ACK 用例 | 旧 ACK 成功曾清掉新账号目标；现在 actor/session fence 拒绝迟到作用 |
| `interactions.test.tsx` 的焦点卡挂载/滚动后确认 | 焦点卡原先没有 row ref，无法 ACK；现在与普通/已完成行使用同一实际挂载确认规则 |
| `valid_native_clicks_present_for_unknown_consumed_and_legacy_routes` | 精确命令复现 `native click was discarded: Route(...)`；去掉早退后，未知/已消费/旧 identifier 都唤起但不猜任务、不改任务文件 |
| `macos::tests::native_exception_is_redacted_as_unavailable` | 生产调用 seam 未设 native catch 时不能返回通用不可用错误；使用合成 Objective-C throw + 外层保护器可安全复现失败，添加 native catch 后通过 |
| `notification_routes_recover_after_real_process_kills` | child 的私有目录守卫拒绝默认 0755 夹具；只把自己新建的 parent 夹具目录改为 0700，**没有删除/放宽守卫**，三个阶段全部通过 |

新增补充守卫：损坏路由仍展示工作区但绝不覆盖；Rust callback panic 不跨 FFI；正常返回/权限错误不被吞成成功；
UUID/URI/XML 输入受约束，未打包 bundle 不触碰通知中心。代码仅让默认点击进入导航，dismiss 的真实系统行为仍待验收。

三个真实通知检查点：

1. Reserved 保存完毕：父进程终止 owned child，新 Routes 仍能接受点击并进入 Pending。
2. Pending 保存完毕：终止后保留待定位，读取本身不消费，ACK 后才变为 Consumed。
3. Consumed 保存完毕：终止后旧点击不能再次取得任务，不复用已消费 UUID。

父进程只对自己 spawn 的 Child 调用 kill/wait，Unix 断言 SIGKILL；helper 要求全新私有合成目录和固定 marker，
没有读取旧任务、UserDefaults、凭据或启动任何旧应用。异常测试只使用内存合成对象；没有向 OS 发送通知或申请权限。

## 4. 真实 Go/MySQL 的独立证据

报告：[`go-mysql-2026-09-11-notifications.json`](go-mysql-2026-09-11-notifications.json)，执行于 15:04:32–15:04:51 +0800。
原 Go 运行时源码 SHA-256：`61a2c105493afc0c7d31e0bd2c9ee622294671b0026e805ceeae4f489b7ece33`，前后保持一致。
workflows / process-restart 均通过，两个阶段都 cleanShutdown；六个 owned PID 随后用 signal 0 只读探测确认不在运行，临时数据已清理。
报告不含 Token/密码/DSN。它验证原 Go 路由/SQL/协调器，不代替系统安全存储或双端原生 UI。

## 5. 尚不能据此宣布完成

- 真实授权弹窗/权限变更、通知横幅/通知中心/声音、macOS/Windows 安装态热冷点击；当前没有这些原生验收证据。
- Windows 完整桌面构建/安装、最低 macOS/Intel；不能用通过的 core/native cross-check 掩盖 SDK 缺口。
- 真多屏/tray rect、NSPanel/Spaces、IME、系统凭据、旧用户域/登录项、性能及旧 45 行为映射。
- 正式 HTTPS、版本/源码标识、签名/公证、各架构安装升级卸载、权限/日志/依赖审计与交付检查表。
- P6 仍需完整验收后用户明确批准；本轮不切换、停用或移除旧应用。
