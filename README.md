# Doing · Tauri 2 客户端

按《tauri2-refactor-plan.md》（原仓库 `doing/docs/`）以 Tauri 2 重构 Doing 任务客户端。
本目录独立于旧 Swift 工程（`../doing`），旧工程保留为行为参照与回滚来源。

- 开发与验收状态：本文件下方「当前状态对照计划」。
- 面向用户的操作说明：`docs/manual.md`（P5 交付物，随实现同步维护）。

## 技术栈

- 桌面运行时：Tauri 2（macOS WKWebView / Windows WebView2）
- UI：React 19 + TypeScript + Vite（主面板单 WebView 常驻；设置窗口按需创建）
- 业务核心：Rust `crates/doing-core`（无平台依赖，可独立测试）
- 服务端：沿用旧 Go + MySQL（`../doing/server`）；客户端只经 Rust 访问 `/api/v1`

## 目录

```text
crates/doing-core/   领域模型、排序、焦点、历史、原子 JSON 持久化（schemaVersion=1）
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
cargo test -p doing-core  # 核心行为测试（当前 42 项）
cargo test -p doing-core -- --nocapture
./scripts/exercise-disk-full.sh  # 磁盘满演练（macOS，1MB 小卷 ENOSPC）
```

## 架构要点（与计划 §3 对齐）

- **Rust 是业务状态唯一写入方**：任务命令经 `engine::mutate_core` 原子落盘（任务与同步元数据
  同一提交边界，`data.json` + `data.json.bak` 已知良好备份），成功后广播快照事件。
- **同步引擎**：epoch 代次使旧任务失效；session 代次隔离跨账号结果；push 串行锁；
  409 → 拉取云端候选 → 冲突面板（本地/云端/稍后决定）；自动同步防抖 2s；
  失败退避由全局 retry driver 驱动；未取得基线前绝不 PUT。
- **凭据**：系统安全存储（macOS Keychain），失败可见报错，不回退明文（无 token.json 降级）。
- **前端为只读投影**：不直接访问 Go API，只消费 `doing://` 事件与命令响应；
  草稿/编辑态在窗口形态切换间保留（WebView 不重建）。
- **迁移**：macOS 检测旧 `items.json` → 备份 → 解析（focusID/notifiedDueIDs 映射）→
  原子提交；绝不写回旧文件；新版已有数据时拒绝自动导入。
- **重启恢复**：启动时若凭据有效，按 `meta` 归属判定——同归属且有未上传修改 → 自动续传；
  本地空 → 云端恢复/标记基线；无归属或换号 → 冲突面板挂起（绝不自动上传/覆盖）。
- **类型契约（计划 P1「禁止长期手写两份契约」已落地）**：Rust DTO 经 `ts-rs` 生成
  `src/types.gen.ts`（生成器 + 漂移守卫在 `src-tauri/src/bindings.rs`，任何 DTO 改动
  未重新生成都会使 `cargo test` 失败）；`src/types.ts` 只做再导出。
- **事件/命令契约**：事件名与命令清单仍两侧手写，由 `src/lib/contract.test.ts`（生成物
  完整再导出、camelCase 抽样、命令四方一致）与 Rust `build.rs` 清单共同守护。

## 当前状态对照计划 §11.2（诚实清单）

自动化测试现状（`cargo test` + `pnpm test`）：
- `doing-core` 42 项（31 单测 + 11 行为映射集成；另有 1 项磁盘满演练 `#[ignore]`，由 `scripts/exercise-disk-full.sh` 按需运行）
- `src-tauri` 33 项（12 同步引擎「真实 TCP 脚本服务器」用例 + 3 API 客户端「刷新轮换/凭据」用例 + 12 迁移用例 + 3 提醒调度/映射用例 + 2 多显示器定位几何用例 + 1 生成物漂移守卫；另有 1 项按需重新生成 `#[ignore]`）
- 前端 Vitest 54 项（DueStamp 相对文案 / 预设跨午夜与 DST / nextHour / 快照合并守卫 /
  **组件交互层（Testing Library + jsdom，mock IPC）**：录入/完成/保存失败提示/行内编辑/截止弹层、
  冲突面板「先选择后确认 + 暂缓不覆盖」、登录校验与服务端拒绝恢复、设置页回滚导出与外观保存、
  **旧版 37 项原生交互检查的可自动化子集**（点击选中/↑↓ 导航/空格完成/回车编辑/⌘↩ 提交/Escape 取消
  与草稿清空/退格删除/已完成折叠/行菜单设焦点与删除/⌘N 聚焦/⌘Z 撤销/登录注册回车提交与各自接口仅一次）、
  **系统集成事件回放**（迁移横幅→导入、`doing://open-settings` 板块一次性请求与设置页消费、
  `session-lost`→登录视图、⌘W/⌘,、模式切换）、**IME 组合守卫**（组合期间回车不提交、结束后提交） / **Rust↔TS 契约守护**：
  事件名、生成物完整再导出与 camelCase 抽样、同步状态枚举、invoke 命令与
  `generate_handler!` 双向一致、**build.rs 命令清单 ↔ capability allow 列表双向一致**）

同步引擎用例覆盖：无基线 GET→PUT(base=0)、PUT 409→冲突挂起（自动上传被阻断）→选择本地以最新版本为基线上传、**结果未知 PUT（服务器已应用但响应丢失）→failed 保留 dirty 与基线 → 重试沿用原基线 → 409 → 冲突挂起、本地数据零丢失（不盲写）**、选择云端替换本地并清史、401→清凭据→unauthorized、503→failed→retry driver 恢复、自动同步关仅本地+手动 flush、会话重置丢弃在途结果、重启恢复（同归属续传 / 无归属转冲突不传 / **未决冲突重启零请求保持**）、云端恢复期间本地编辑→转冲突候选（新工作保留）。**该测试套件在首跑中发现并修复了一个真实生产死锁**（`match st.engine.read().await.known_version` 的匹配临时将读锁存活跨越了分支内 await，随后的 `engine.write()` 自死锁）。

| 检查项 | 状态 |
| --- | --- |
| P0 高风险原型 | macOS 本机运行验证（登录页/工作区/托盘/单实例均实测）；Windows/Intel/旧系统缺设备未验证 |
| 旧行为测试映射 | 域模型 42 项 + 应用层 33 项全绿；**旧版 37 项原生交互检查已建立映射**：16 项可自动化子集由前端组件测试覆盖（导航/编辑/删除/折叠/行菜单/快捷键/登录注册），IME 组合的逻辑守卫（组合期间提交抑制）另已自动化，真机候选窗交互与 Cmd+A 选择、系统菜单路径标注为人工验收；服务端契约 E2E 仍待 MySQL 环境 |
| 迁移/回滚演练 | 导入实现 + 导出实现（兼容旧格式，位于 `exports/`，往返解析测试）共 12 项自动化测试；写入故障矩阵新增覆盖：**磁盘满**（1MB 小卷 ENOSPC 演练通过，`scripts/exercise-disk-full.sh`）、**无写权限**（repo 层 + 导入层双层自动化）、**崩溃残留 tmp / 伪中断**（load/save 不受影响）与**损坏目标拒写**；未覆盖：真实 kill -9 时序注入、Windows 侧同矩阵 |
| IME/快捷键/模式切换/DPI | 组合输入**逻辑守卫**已有自动化测试（组合期间回车不提交 / 结束提交，录入框与行内编辑器双覆盖），快捷键/模式切换由组件测试与 App 事件回放覆盖；**键盘注入部分可行**（实测：⌘N/⌘,/⌘A/⌫/Esc/字母数字均生效；**Return 无法送达 WebView 输入框、CJK 字符回退为 'a'**，因此“键盘录入提交”仍须真实键盘人工验收；见 `scripts/e2e-keyboard.sh` 头部结论）；托盘点击不可注入（见下）；IME 候选窗与 DPI 仍待人工 |
| 双端截图验收 | macOS 实机截图已入库：`docs/screenshots/`（登录页浅色/深色、工作区「含逾期与同步失败态」、启动仲裁冲突面板、设置窗口通用页；dev 构建 + 启动即显示采拍，release 与 dev 共用同一前端与样式）；Windows 缺设备（本机交叉检查受 ring/cc 需 Windows C 工具链限制不可行） |
| P5 交付物（部分） | ✅ DMG 构建与挂载/拷贝运行/删除演练、用户操作手册（`docs/manual.md`）、已知限制（本文件缺口清单）；❌ 签名/公证与版本-源码标识（等 D07 与版本控制）、安装/升级/卸载的干净机器复验、Windows 安装包 |
| 服务端 E2E | 需要 MySQL/服务端环境（本机无 docker/mysql） |

**发布版性能初测（非正式；macOS arm64，release 包，采样脚本见 `scripts/perf-*.sh`）**：
- 空闲 5 分钟（静止登录页，20 样本）：主进程 RSS 82–111 MB、WebKit WebContent 89–148 MB
  （末值 ~86/110 MB，呈缓存回收后回落）；瞬时 CPU ≈0%。
- **内存拆分（`scripts/perf-memory-breakdown.sh`，启动差集归属 WebKit XPC 进程；
  全进程空闲 CPU 0.0%）：5 个常驻进程 t≈30s → 5 min 的 `phys_footprint`（括号内 RSS）**：
  主进程（Rust 壳）32 → **33 MB**（108.7 → 94.7）；主窗渲染 19 → **21 MB**（51.2 → 31.1）；
  设置窗渲染 30 → **32 MB**（48.2 → 27.3）；GPU 12 → **15 MB**（39.8 → 28.1）；
  Networking 5.7 → **6.6 MB**（14.1 → 9.4）。合计 phys_footprint ≈ **108 MB**、RSS ≈ 190 MB
  ——RSS 含共享页，拆分后实际私有占用约为总和的一半；设置窗口因启动时即预载而存在第二个渲染进程。
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
3. 托盘锚点仍为「当前显示器右上」近似（Tauri 无托盘坐标 API；多屏偏移/负坐标已修并单测）；
   失焦收起 + IME 候选窗豁免需真机输入法实测（已按旧版 NSPopover 语义置顶）。
   托盘按钮由系统状态栏承载：AX/合成点击不会产生真实 NSEvent，托盘交互需**人工点击**完成最终验收
   （点击处理已兼容 Down/Up 并做 250ms 去抖）；应用窗口的合成键盘输入不受此限制（已实测，见 IME 行）。
4. 通知链路：**投递已实测**（dev 实机：`permission_state=Granted`，两条到期任务 `show()` 提交成功，
   授权/投递日志取证）；**点击→定位任务**仍待人工单击横幅验证（系统横幅点击无法脚本注入；
   `onAction` 转发与数字 id→任务映射已有实现与单测覆盖逻辑侧）。
5. WebdriverIO Tauri E2E（计划 §10.1 的 tests/e2e 目录）未接入（P0 选型任务）；当前 UI 证据为
   截图+辅助功能读取+真实 HTTPS 契约联调（mock），命令/状态机由 Rust 单测覆盖。
6. 服务地址默认开发 `http://127.0.0.1:8080`（`DOING_API_URL` 可覆盖）；正式地址待定（D06）。
7. 仓库已初始化并推送：`https://github.com/zxcodenb/doing-tauri`（public，`main`）。
   CI 工作流 `.github/workflows/ci.yml` 因 gh OAuth 缺 `workflow` scope 暂未入库（文件保留在本地，
   授权后补交一次即可）；P5 的“版本/源码标识”自此可基于 commit 落地。

**近期修复记录（均由自动化测试/实机验收驱动）**：
- 修复同步引擎真实死锁（读锁经 `match` 临时跨 `await`，见上）。
- 统一 auth→core 锁序，消除 ABBA 风险（`maybe_auto_sync`/`resume_after_restart`）。
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

## 本地联调（无服务端）

```bash
python3 scripts/dev-mock-server.py          # 127.0.0.1:8080，提供 /api/v1 契约（GET 云快照 / PUT 409）
DOING_SHOW_ON_LAUNCH=1 DOING_SKIP_LOGIN=1 pnpm tauri:dev
# 本地非空 + 云端非空 → 启动即进入「保留哪一份？」冲突面板（端到端验证同步仲裁）
# 令牌经 DOING_DEV_TOKEN 注入内存凭据（默认 mock-access），不触碰系统 Keychain
```

## 类型契约

DTO 载荷由 Rust 单侧定义并生成：`cargo test -p doing-desktop --lib regenerate_types_gen -- --ignored`
重新生成 `src/types.gen.ts`（ts-rs；u64/i64 → `number`，Option → `| null`，serde camelCase 透传）。
漂移守卫 `types_gen_is_up_to_date` 随常规测试运行，DTO 改动未重新生成即失败（已实测：手动破坏
生成物 → 失败；重新生成 → 通过）。`src/types.ts` 仅再导出生成类型并保留展示常量（`SYNC_TEXT`）。
事件名与命令清单仍为手写契约，由 `contract.test.ts` + `build.rs` 清单 + capability allow 列表
四方一致守卫（ACL 负向实测见上文）。
