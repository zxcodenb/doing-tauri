# Doing · Tauri 2 客户端

按《tauri2-refactor-plan.md》（原仓库 `doing/docs/`）以 Tauri 2 重构 Doing 任务客户端。
本目录独立于旧 Swift 工程（`../doing`），旧工程保留为行为参照与回滚来源。

- 最新代码复核、修复证据与剩余门禁：[`docs/migration-progress.md`](docs/migration-progress.md)。
- 历史开发与验收记录：本文件下方「当前状态对照计划」；历史绿灯不等于整个迁移计划已验收。
- 面向用户的操作说明：`docs/manual.md`（P5 交付物，随实现同步维护）。

## 技术栈

- 桌面运行时：Tauri 2（macOS WKWebView / Windows WebView2）
- UI：React 19 + TypeScript + Vite（主面板单 WebView 常驻；设置按需创建、关闭销毁，原生重开行为仍需实机复验）
- 业务核心：Rust `crates/doing-core`（无平台依赖，可独立测试）
- 服务端：沿用旧 Go + MySQL（`../doing/server`）；客户端只经 Rust 访问 `/api/v1`

## 目录

```text
crates/doing-core/   领域模型、排序、焦点、历史、原子 JSON 持久化（schemaVersion=2）
crates/doing-notifications/  薄原生通知适配器：权限、OS 接受、opaque 点击标识
src-tauri/           应用层：命令/IPC、认证与同步引擎、托盘/窗口、提醒、迁移
src/                 React 前端：工作区、设置、登录、冲突面板、设计系统
docs/manual.md       用户操作手册（P5 交付物）
docs/screenshots/    实机截图证据（登录/工作区/冲突/设置）
server/              仍在旧仓库（本工程只引用协议）
```

## 常用命令

```bash
pnpm install
pnpm tauri:dev            # 开发运行（macOS 本机）
pnpm tauri:build          # 构建（签名/公证条件见计划 D07）
cargo test -p doing-core  # 核心行为测试（当前 55 项）
cargo test -p doing-core -- --nocapture
./scripts/exercise-disk-full.sh  # 磁盘满演练（macOS，1MB 小卷 ENOSPC）
```

## 架构要点（与计划 §3 对齐）

- **Rust 是业务状态唯一写入方**：任务命令经 `engine::mutate_core` 原子落盘（任务与同步元数据
  同一提交边界，`data.json` + `data.json.bak` 已知良好备份），成功后广播快照事件。
- **同步引擎**：会话/操作代次与串行网络流程；已确认基线与未决候选分离；PUT 前持久化不确定标记，
  409/未知写入先 GET 并仲裁，不盲目重放；自动同步防抖 2s、退避上限 60s，所有自动路径受设置开关约束。
- **凭据**：系统安全存储 + 绑定随机会话 ID 的租约，旧账号网络结果不能读写新账号凭据；失败可见，不回退明文。
  `auth-session.json` 只存随机 ID/null，不含 Token 或用户名；旧 token-only 记录需重新登录。
- **前端为只读投影**：等待全部监听器注册后握手，按会话代次/事件修订合并；云端 i64 版本以字符串跨 IPC；
  工作区按账号会话重建，切换窗口形态不重建，冲突候选以 UUID + 版本共同确认。
- **偏好/提醒**：设置和坐标采用独立版本化原子仓库；原生通知被 OS 接受后才记录已提醒。UUID 路由先落盘，点击队列跨重启保留，登录/归属门禁通过且 UI 实际定位后才 ACK；协议见 [`docs/notification-delivery.md`](docs/notification-delivery.md)。
- **迁移**：macOS 检测旧 `items.json`，确认后备份任务与可选白名单 UserDefaults → 解析/逐字段核对 →
  journal 驱动的双文件原子发布/恢复/撤销；绝不写回旧来源，已有新版文件拒绝首次导入覆盖。
  未完成时阻止写入与认证恢复，导入后重新登录并仲裁归属；协议见 [`docs/migration-transaction.md`](docs/migration-transaction.md)。
- **重启恢复**：身份从安全记录恢复，数据归属使用规范化服务地址 + JWT 稳定账号 ID（不替代服务端鉴权）。
  已变化的基线、未决候选、未知上传结果都会重新仲裁；dirty 空列表是删除，不因“本地为空”而自动恢复。
- **类型契约（计划 P1「禁止长期手写两份契约」已落地）**：Rust DTO 经 `ts-rs` 生成
  `src/types.gen.ts`（生成器 + 漂移守卫在 `src-tauri/src/bindings.rs`，任何 DTO 改动
  未重新生成都会使 `cargo test` 失败）；`src/types.ts` 只做再导出。
- **事件/命令契约**：事件名与命令清单仍两侧手写，由 `src/lib/contract.test.ts`（生成物
  完整再导出、camelCase 抽样、命令四方一致）与 Rust `build.rs` 清单共同守护。

## 当前状态对照计划 §11.2（诚实清单）

> 2026-09-11 当前工作区已验证任务/偏好事务、持久化候选、认证租约、同步竞态、IPC 握手、提醒提交顺序和无覆盖原子导出。
> P4 已补双文件迁移 journal、白名单偏好、恢复 UI、20 检查点真实 SIGKILL、工作区坐标逻辑，以及原生通知/持久定位/ACK 和三个通知进程终止检查点；本机真实隔离 Go/MySQL 两阶段联调再次通过。
> **P0–P6 全部目标仍未完成**：系统安全存储、双端原生/安装验收、旧用户域与多屏实机语义、登录项处理、通知安装态热冷点击和正式发布/切换条件仍未齐备。
> 具体证据、版本格式和下一步见 [`docs/migration-progress.md`](docs/migration-progress.md)。

本轮完整验证：
- `cargo test --workspace --locked`：核心 **55**、应用层 **144**、原生通知库 **7** 项通过，共 **206** 项。
- `pnpm run ci`：**102** 项 Vitest、TypeScript、Vite 构建通过（8 条 React 建议级 warning，无构建失败）。
- Rust Clippy `-D warnings`、Windows **核心与原生通知库**交叉检查、临时卷 ENOSPC 演练通过。
- Windows 完整桌面 cross-check 未通过：ring C 编译缺少 Windows SDK `assert.h`；不把局部通过当作 Windows 壳已验证。
- 本轮 macOS arm64 release 桌面二进制编译通过（4m25s）；未启动、打包安装或进行原生验收。
- 真实 Go/MySQL 两阶段受保护入口及 7 项 Python 启动器守卫通过；报告见 [`go-mysql-2026-09-11-notifications.json`](docs/evidence/go-mysql-2026-09-11-notifications.json)。
- 忽略的类型生成/磁盘满/真实数据库入口与两个私有 kill helper 另行受控执行；迁移 20 + 通知 3 个子进程不重复加算到常规 206 项。
- 本轮没有安装或运行新 release 产物；详见 [`通知重构证据`](docs/evidence/notifications-2026-09-11.md)。旧 [`P4 证据`](docs/evidence/p4-migration-2026-09-11.md) 保留其历史结果。

### CI 首轮（2026-09-14，不改变上述 P0–P6 未完成结论）

- `main` 上两轮 Actions 5/5 全绿：`a347cee`（工作流入库，run `34794672684`）与 `75d080e`（actions 升到 Node 24 大版本、Windows 校验文件改 LF，run `34795422690`）。
- 覆盖：前端 **102** 项 Vitest + lint + `tsc/vite`；Rust 核心 55 项 + clippy `-D warnings`；应用层 workspace **206** 项（口径与本地一致，5 项受控 `ignored`）；ts-rs 漂移守卫在 CI 生效。
- 产物由 runner 构建：arm64 DMG（3.41 MB）与 x64 NSIS（3.02 MB）+ `SHA256SUMS.txt` 作为 artifact 保留 30 天；下载后 `shasum -c` 与 `hdiutil imageinfo` 本机复核通过，两次 run 哈希不同（不宣称可复现构建）。
- **原生 Windows runner 上完整桌面壳编译 + NSIS 打包通过**，解除 2026-09-11 记录的 macOS 交叉编译 ring/Windows SDK 阻塞；仅构建层，Windows 运行验收仍缺。
- 边界：产物未签名未公证（不等于 D07）；CI runner 不替代托盘点击、IME、通知横幅、多屏、干净机安装等实机验收（计划 §11.1）；无 universal/Intel 包。详见 [`CI 首轮证据`](docs/evidence/ci-first-run-2026-09-14.md)。

### 较早原型的历史记录（不作为本轮重写产物的验收）

以下保留既有原型截图、性能和安装演练，不能据此推断当前代码已在原生桌面验证。

| 检查项 | 状态 |
| --- | --- |
| P0 高风险原型 | macOS 本机运行验证（登录页/工作区/托盘/单实例均实测）；Windows/Intel/旧系统缺设备未验证 |
| 旧行为测试映射 | 域模型 48 项 + 应用层 43 项全绿；**旧版 37 项原生交互检查已建立映射**：16 项可自动化子集由前端组件测试覆盖（导航/编辑/删除/折叠/行菜单/快捷键/登录注册），IME 组合的逻辑守卫（组合期间提交抑制）另已自动化，真机候选窗交互与 Cmd+A 选择、系统菜单路径标注为人工验收；服务端契约 E2E 仍待 MySQL 环境 |
| 迁移/回滚演练 | 导入实现 + 导出实现（兼容旧格式，位于 `exports/`，往返解析测试）共 12 项自动化测试；写入故障矩阵新增覆盖：**磁盘满**（1MB 小卷 ENOSPC 演练通过，`scripts/exercise-disk-full.sh`）、**无写权限**（repo 层 + 导入层双层自动化）、**崩溃残留 tmp / 伪中断**（load/save 不受影响）与**损坏目标拒写**；未覆盖：真实 kill -9 时序注入、Windows 侧同矩阵 |
| IME/快捷键/模式切换/DPI | 组合输入**逻辑守卫**已有自动化测试（组合期间回车不提交 / 结束提交，录入框与行内编辑器双覆盖），快捷键/模式切换由组件测试与 App 事件回放覆盖；**键盘注入部分可行**（实测：⌘N/⌘,/⌘A/⌫/Esc/字母数字均生效；**Return 无法送达 WebView 输入框、CJK 字符回退为 'a'**，因此“键盘录入提交”仍须真实键盘人工验收；见 `scripts/e2e-keyboard.sh` 头部结论）；托盘点击不可注入（见下）；IME 候选窗与 DPI 仍待人工 |
| 双端截图验收 | macOS 实机截图已入库：`docs/screenshots/`（登录页浅色/深色、工作区「含逾期与同步失败态」、启动仲裁冲突面板、设置窗口通用页；dev 构建 + 启动即显示采拍，release 与 dev 共用同一前端与样式）；Windows 缺设备（本机交叉检查受 ring/cc 需 Windows C 工具链限制不可行） |
| P5 交付物（部分） | ✅ DMG 构建与挂载/拷贝运行/删除演练、用户操作手册（`docs/manual.md`）、已知限制（本文件缺口清单）；❌ 签名/公证与版本-源码标识（等 D07 与版本控制）、安装/升级/卸载的干净机器复验、Windows 安装包 |
| 服务端 E2E（历史） | 当时缺少 MySQL；2026-09-11 已补本机隔离 Go/MySQL 真实协调器联调，见上方当前证据，仍不是双端原生 E2E |

**发布版性能初测（非正式；macOS arm64，release 包，采样脚本见 `scripts/perf-*.sh`）**：
- 空闲 5 分钟（静止登录页，20 样本）：主进程 RSS 82–111 MB、WebKit WebContent 89–148 MB
  （末值 ~86/110 MB，呈缓存回收后回落）；瞬时 CPU ≈0%。
- **内存拆分（`scripts/perf-memory-breakdown.sh`，启动差集归属 WebKit XPC 进程；
  全进程空闲 CPU 0.0%）：5 个常驻进程 t≈30s → 5 min 的 `phys_footprint`（括号内 RSS）**：
  主进程（Rust 壳）32 → **33 MB**（108.7 → 94.7）；主窗渲染 19 → **21 MB**（51.2 → 31.1）；
  设置窗渲染 30 → **32 MB**（48.2 → 27.3）；GPU 12 → **15 MB**（39.8 → 28.1）；
  Networking 5.7 → **6.6 MB**（14.1 → 9.4）。合计 phys_footprint ≈ **108 MB**、RSS ≈ 190 MB
  ——RSS 含共享页，拆分后实际私有占用约为总和的一半；该历史产物的设置窗口启动预载，故有第二个渲染进程；本轮已改为懒加载，不能沿用这些数值声称新内存开销。
- 冷启动至菜单栏托盘就绪（进程拉起→托盘项可访问，**30 次**）：P50 800 ms、P95 1097 ms（min 571 / max 2064）。
- 二次启动唤起至窗口出现（单实例转发→present，**30 次**）：P50 534 ms、P95 765 ms（min 370 / max 798；含新进程启动，为热打开上界）。
- 对照计划初值：菜单栏应用“就绪”路径显著优于 3s 冷启动目标方向；拆分层已补齐
  （共享/私有归因、常驻进程清单）；仍缺——首次 WebView 初始化的单独计时（release 下首窗出现
  需人工点击托盘，无法脚本注入）与 Windows 侧同口径对照；`pnpm lint` 仅剩 React 建议级告警，无 error。

**已知缺口/风险**：
1. Windows、Intel macOS、旧系统版本未验证（需真实设备）。
2. 自定义命令 ACL 已按官方方式登记（`build.rs` `AppManifest::commands` + capability 逐条 allow），
   四方一致性由契约测试守卫；**负向运行时验证已完成**：临时移除 `allow-init-state` 后应用启动停在
   「Doing 启动中…」（命令被运行期拒绝），恢复授权后正向回归通过。
3. 托盘锚点仍是显示器边缘近似；浮窗缩放/工作区算法已有隔离测试，实际多屏仍需复验；当前实现尚未消费 TrayIconEvent 提供的 rect/position，不能再归因为“Tauri 无坐标 API”；
   失焦收起 + IME 候选窗豁免需真机输入法实测（已按旧版 NSPopover 语义置顶）。
   托盘按钮由系统状态栏承载：AX/合成点击不会产生真实 NSEvent，托盘交互需**人工点击**完成最终验收
   （点击处理已兼容 Down/Up 并做 250ms 去抖）；应用窗口的合成键盘输入不受此限制（已实测，见 IME 行）。
4. 通知链路：较早 dev 原型只记录旧插件 `Granted` / `show() == Ok`；本轮源码复核确认这些返回值不能证明真实 OS 授权或接受，不能继续作为投递验收。
   现已改为原生权限/提交、持久 UUID 路由和 UI ACK，并有跨重启/进程终止回归；**真实横幅、声音及安装态热/冷点击仍未验收**。
5. WebdriverIO Tauri E2E（计划 §10.1 的 tests/e2e 目录）未接入（P0 选型任务）；当前 UI 证据为
   截图+辅助功能读取+本机 HTTP mock 契约联调，命令/状态机由 Rust 单测覆盖。
6. 开发地址默认 `http://127.0.0.1:8080`，release 默认 `https://api.invalid.invalid` 占位（不可交付为正式服务）；`DOING_API_URL` 受控环境配置与正式 HTTPS 地址仍待 D06 审查。
7. 仓库已初始化并推送：`https://github.com/zxcodenb/doing-tauri`（public，`main`）。
   CI 工作流 [`.github/workflows/ci.yml`](.github/workflows/ci.yml) 已入库（`a347cee`）并两轮全绿：
   前端/Rust 核心门禁、macOS 应用层 206 项与 clippy、arm64 DMG 与 x64 NSIS 产物（含 SHA-256）
   均由 runner 构建；证据见 [`docs/evidence/ci-first-run-2026-09-14.md`](docs/evidence/ci-first-run-2026-09-14.md)。
   P5 的“版本/源码标识”自此可基于 commit 落地；产物未签名未公证，不代表 D07 通过。

**近期修复记录（均由自动化测试/实机验收驱动）**：
- 修复同步引擎真实死锁（读锁经 `match` 临时跨 `await`，见上）。
- 历史上统一 auth→core 锁序（本轮进一步统一 engine→auth→core），消除 ABBA 风险（`maybe_auto_sync`/`resume_after_restart`）。
- 启动恢复仲裁补齐：同归属续传 / 无归属或换号进入冲突（不自动上传）。
- 多显示器定位修复：叠加显示器全局原点（含负坐标副屏），菜单栏形态/浮窗形态分别修正并补单测。
- 菜单栏形态窗口置顶（对齐旧 NSPopover 浮层语义）；主动唤起后 1.2s 失焦宽限，避免抢焦点失败即自收起。
- 单实例二次启动与托盘点击统一走 `present_main`（定位/置顶/焦点一致）。
- 托盘点击事件兼容 Down/Up 派发并 250ms 去抖（真实鼠标与辅助技术路径统一）。
- 托盘「从云端恢复…」→ `doing://open-settings` 链路补齐（打开设置并定位到账户板块）。
- 托盘菜单与设置窗口实机验收：经辅助功能读取托盘菜单全部项（含动态「同步状态：空闲」），
  菜单栏标题实测显示「 ⏰ 已逾期」；点击「设置…」打开 760×610 设置窗口并截图核对。
- 启动握手修复（计划 §3.4）：前端**先订阅事件再拉初始快照**，快照按 revision 防回退；
  `init_state` 随握手返回冲突候选（弥补订阅前丢失的 `doing://conflict`）——修复前冲突面板
  在“启动即仲裁”场景不显示（实测复现并修复）。
- 本地联调链路修复：开发默认地址显式 `127.0.0.1`（规避 localhost 双栈悬挂导致请求未达）；
  `DOING_SKIP_LOGIN=1` 联调模式使用**内存凭据**（`DOING_DEV_TOKEN`，绝不写入系统 Keychain），
  配合 `scripts/dev-mock-server.py` 完成「启动仲裁 → 冲突面板」端到端实测（GET v9 → askUser）。
- 发布配置修复：`[profile.release]`（LTO/opt-s/strip）原位于 workspace 成员 `src-tauri/Cargo.toml`
  会被 Cargo 忽略，已移至 workspace 根使发布优化真正生效：主程序 **17.8 → 7.7 MB**、DMG **5.05 → 3.1 MB**。
- Rust 质量清理：clippy 告警归零（含类型别名、`to_local` 命名、`Default` 初始化风格等），CI 已启用
  `clippy -- -D warnings` 强制。
- 发布包安装演练：DMG 挂载（卷内 Applications 链接 + Doing.app，Info.plist 校验通过）→ 拷贝到独立
  路径运行（首次冷启动 3.1 s 含 Gatekeeper 评估，托盘就绪；二启唤起窗口正常；浅色主题登录页截图）→
  删除副本不影响应用数据目录（数据在 Application Support，卸载不清理用户数据）。
- 主题两态证据齐备：工作区/登录页/设置窗口在深色与浅色（跟随系统）下均有实机截图核对。
- 前端交互测试层落地（计划 §10.1）：Testing Library + jsdom，mock Tauri IPC 后以真实 Provider
  驱动组件；随之修复 Workspace 提示条缺少 `role=alert/status` 的可访问性缺口。
- ACL 负向端到端实测（计划 R5 完成）：临时移除 `allow-init-state` → 应用停在「Doing 启动中…」，
  恢复授权后正向回归通过；契约测试持续守卫四方一致性。
- 回滚导出落地（计划 §7.4）：设置-数据页「导出兼容旧版（回滚用）…」→ 旧格式文件写入
  `exports/` 并在 Finder 揭示；导出一份数据必须能被旧格式解析器读回（往返测试）。
- 持久化故障矩阵补测（计划 §7.2）：原子提交在 tmp 写入失败（磁盘满）时会清理自身残留 tmp、
  已提交主文件零改动；新增「无写权限不损坏主文件」「崩溃残留 tmp 不影响 load/save」单测，
  并以 1MB 小卷完成真实 ENOSPC 演练（`scripts/exercise-disk-full.sh`，实测通过）。
- 键盘交互模型修复（映射旧版 37 项原生交互检查时发现）：此前方向键/空格/回车/退格仅在
  焦点离开录入框时可用，实际不可达。现改为——任务行可聚焦（选中即移交焦点）、↑↓ 在录入框内
  也可导航、点击编辑器不再抢走输入焦点、数据就绪后才首次聚焦录入框；
  随之补齐 16 项组件测试（导航/编辑/删除/折叠/行菜单/快捷键/登录注册），前端测试 46 项。
  改动后 dev 实机冒烟通过：应用在副屏正常渲染，并弹出启动仲裁「保留哪一份？」冲突面板
  （本地 4 项 / mock 云端 1 项，Rust↔WebView 事件链与 mock 服务端联通正常）。
- 类型契约生成落地（计划 P1）：`src-tauri/src/bindings.rs` 用 ts-rs 生成 `src/types.gen.ts`
  （17 个 DTO：事件载荷 + 命令参数），`src/types.ts` 改为再导出；`ipc.ts` 各命令参数以生成类型
  校验（`satisfies`）。生成器不依赖 ts-rs 的同文件合并（实测其多类型合并对枚举会整文件重建，
  缺失既有声明），改为逐类型 `export_to_string` 按固定顺序拼装。漂移守卫已实测有效
  （手动破坏生成物 → 测试失败；重新生成 → 通过）。
- 性能证据细化（计划 D08）：新增 `scripts/perf-memory-breakdown.sh`——以“启动前后 WebKit XPC
  进程差集”归属 WebView 子进程，输出每个常驻进程的 `phys_footprint`/峰值/RSS。实测
  （release、静止登录页、全进程 CPU 0.0%）：5 常驻进程 5 分钟空闲合计 phys_footprint ≈108 MB、
  RSS ≈190 MB；RSS 含共享页，拆分后私有占用约为其一半。仍缺首窗出现的人工点击计时与 Windows 对照。
- 系统集成事件回放测试（+8 项，前端 54 项）：迁移横幅→`migration_import`、`doing://open-settings`
  一次性板块请求（App 写入 → SettingsApp 消费并清除）、`session-lost`→登录视图、⌘W/⌘,、
  模式切换（钉在桌面）→`system_toggle_mode`；并为 IME 组合守卫补自动化：录入框与行内编辑器
  在组合期间回车不提交、组合结束后提交（jsdom 回放 composition/isComposing，真机候选窗仍人工）。
- 同步不变量补测（计划 P3「结果未知 PUT 不丢数据或越权覆盖」）：脚本服务器新增“响应丢失”
  约定（status 0 = 收到请求后直接断开）；用例验证——failed 且保留 dirty/基线、重试沿用原基线
  → 409 → 冲突挂起、本地数据零丢失（服务器已应用也不盲写）。
- API 客户端刷新链路补测（计划 §4.3 刷新轮换/单次刷新，+3 项）：过期访问令牌 → 恰一次刷新 →
  保存新凭据 → 原请求重试一次（请求日志断言新旧 Bearer）；刷新失败 → 清凭据 → RefreshFailed
  且不递归重试、原请求不再发出；并发刷新 single-flight（慢刷新窗口内两路共享同一结果，仅 1 次
  刷新请求）。**测试基础设施缺陷一并修复**：脚本服务器的 Bearer 解析在小写化后大小写敏感匹配，
  导致 `Req.bearer` 恒为 None（此前用例从未断言过）；修复后在同步用例中加入“业务请求必须携带
  访问令牌”的断言。
- P5 交付物补全（部分）：新增 `docs/manual.md` 用户操作手册——安装/快捷键总表/同步冲突语义/
  数据位置与原子保存/迁移与回滚/卸载与故障排查/已知限制；所有界面文案与行为均按当前代码核对
  （左键开合面板、右键出菜单、预设与选项文案、数据与设置文件位置等）。
- 交付证据入库与结论更正：`docs/screenshots/` 固化 5 张实机截图（登录页浅/深、工作区含逾期态、
  启动仲裁冲突面板、设置窗口通用页）；复核发现先前「系统级合成键盘不可自动化」的结论仅对托盘成立
  ——本应用窗口的合成键盘事件实测有效（Esc 关闭行内编辑、⌘A+⌫ 清空录入、⌘, 唤起设置均生效），
  已在「IME/快捷键」与「托盘缺口」两处更新表述。

## 本地真实 Go / MySQL 联调

可复现入口与隔离边界：[`tests/go-mysql/README.md`](tests/go-mysql/README.md)。
`python3 scripts/exercise-go-mysql.py --mysql-basedir <MySQL-8.4目录> --server-source ../doing/server`
会建立全新私有 socket-only 数据库并启动原 Go 路由，不使用现有 DB/真实凭据、不注册系统服务。
包含两个 Rust 测试进程，以及 MySQL/Go 的实际退出重启；不代替双端原生桌面/Keychain 验收。

## 本地联调（无服务端）

```bash
python3 scripts/dev-mock-server.py          # 127.0.0.1:8080，提供 /api/v1 契约（GET 云快照 / PUT 409）
DOING_SHOW_ON_LAUNCH=1 DOING_SKIP_LOGIN=1 pnpm tauri:dev
# 本地非空 + 云端非空 → 启动即进入「保留哪一份？」冲突面板（端到端验证同步仲裁）
# 令牌经 DOING_DEV_TOKEN 注入内存凭据（默认 mock-access），不触碰系统 Keychain
```

## 类型契约

DTO 载荷由 Rust 单侧定义并生成：`cargo test -p doing-desktop --lib regenerate_types_gen -- --ignored`
重新生成 `src/types.gen.ts`（ts-rs；服务端 i64 版本跨 IPC 使用十进制 `string`，Option → `| null`，serde camelCase 透传；其他字段以生成物为准）。
漂移守卫 `types_gen_is_up_to_date` 随常规测试运行，DTO 改动未重新生成即失败（已实测：手动破坏
生成物 → 失败；重新生成 → 通过）。`src/types.ts` 仅再导出生成类型并保留展示常量（`SYNC_TEXT`）。
事件名与命令清单仍为手写契约，由 `contract.test.ts` + `build.rs` 清单 + capability allow 列表
四方一致守卫（ACL 负向实测见上文）。
