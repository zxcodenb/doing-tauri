# Tauri 2 迁移：代码复核与继续实施记录

日期：2026-09-11。目标仍是原工程 `../doing/docs/tauri2-refactor-plan.md` 的 **P0–P6 全部目标**。
本记录不是正式发布或真实数据切换批准。旧 Swift 工程、旧数据与旧凭据没有在本轮修改。
复核基线为新工程 `5a9a721`；以下代码与验证来自其后的未提交工作区，没有执行 commit / push。
原有未跟踪 `.github/` 保留。原计划 SHA-256：`e2f96b1f9f07f7902c0131614a365198081c79184f8ccbf4d597680720101e2c`。

## 1. 结论与证据范围

已推进任务/偏好事务、持久化保护、认证租约、同步仲裁、可靠 IPC、提醒投递顺序与设置窗口生命周期。
P4 新增旧任务/可选白名单偏好的双文件恢复日志、来源/进程预检、20 检查点真实 SIGKILL 恢复，以及主/设置窗口的导入确认与恢复 UI。
随后完成原生通知适配器、持久账号路由、UI 定位后 ACK 和通知三个检查点的真实进程终止测试；不再依赖旧通知插件的伪 Granted 与进程内 map。
本机隔离的**真实 Go/MySQL 两阶段联调**已通过，仍不能代替双端原生高风险行为、系统安全存储、旧用户域/登录项实机语义、正式分发和用户切换门禁；**全计划仍未完成**。

已识别回归先在实际路径上复现失败，再修复；没有把旧的不安全断言改成“通过”作为修复：

- 迟到的登录/刷新会在登出后恢复凭据；旧登出的 401 会清掉新账号；迟到的旧 access 401 会多刷新一次。
- 原有“冲突重启”测试只置内存标志；新用例实际销毁状态、从文件新建实例，再验证零越权 PUT。
- 前端没有真正等待监听器注册，主题变化还会重新握手；迟到的初始状态和旧会话事件会回退 UI。
- 云端恢复期间导入旧数据，迟到结果会覆盖刚导入的数据；归档失败会卡在“同步中”。
- 凭据已撤销但认证事件未发布时，任务编辑/等待核心锁的冲突确认仍能提交。
- 偏好写入失败仍返回成功、恢复默认先改内存、损坏/未来版本被覆盖、空补丁也写盘。
- 没有原生投递通道也会标成已提醒；提醒记录还会修改任务时间戳，制造虚假云端差异。
- 固定浮窗、composition 期间及已重新获焦的窗口，会被迟到的失焦检查隐藏。
- 迁移包装层对错误事务 ID、已完成的恢复/保留和已初始化空文件的拒绝导入仍重载数据，清掉撤销历史并推进同步代次。
- 锁定 Tao 的屏幕名称为 `Monitor #<model>`，不是 NSScreen.localizedName；先前依赖名称的转换在真实命名规则下只能返回默认位置。
- 通知路由重启后丢失；焦点卡没有真实 row ref 导致无法 ACK；旧会话 ACK 的迟到成功会清掉新账号目标。
- 有效的未知/已消费通知点击被提前丢弃；原生 Objective-C 异常没有在可恢复错误边界收敛。均有隔离红绿回归。

竞态/异常网络测试使用隔离的并发 TCP 脚本服务器与响应屏障；另有 §2.6 的真实 Go/MySQL 联调，二者证据分开记录。
凭据故障测试只使用合成 Token 和注入的秘密存储后端；
通知业务测试使用注入的投递结果和真实私有文件/进程终止；原生适配器测试仅用 URI/XML、内存合成异常和自身 bundle mismatch，不访问通知中心或触发权限框。
脚本服务器用例验证协调器/原子文件路径；真实数据库用例验证实际 Go 路由/SQL，
**两者都不等于生产部署、系统安全存储或 OS 横幅已经验收**。

## 2. 已实现的安全边界

### 2.1 P1：任务事务与版本化仓库

- 任务、焦点、历史、dirty 与本地修订在核心副本上修改；文件成功提交后才发布。失败/空操作不污染权威状态。
- 主文件与备份使用唯一临时文件、排他创建、内容刷新、同卷原子替换；备份失败阻止主文件提交。
- 普通保存拒绝损坏、旧无版本格式和未知新版本；显式备份恢复保留恢复前文件，不允许降级覆盖未来格式。
- `data.json` 当前为 **schema 2**。支持只读解析 schema 1 并在合法提交时升级；不从旧用户名推断稳定账号归属。
- 同一文件保存：数据 `revision`、`localEditRevision`、dirty、已确认基线、服务地址 + 稳定账号 ID、
  `pendingConflict`（含 UUID、归属、原因和可选候选快照）、`uncertainUpload`（PUT 前的标记）。
- `localEditRevision` 区分任务编辑与提醒记录：提醒改变快照修订，但不改任务 `updatedAt`，不让成功 PUT 错误留下 dirty。
- 仓库初始化检查与发布共享写锁；导入不能覆盖已初始化文件。Windows 原子替换不使用先删后写或跨卷复制降级。

证据：`crates/doing-core/src/{data,repo,store}.rs`、`tests/domain_mapping.rs`、
`src-tauri/src/commands.rs::transaction_tests`。

### 2.2 P3：认证与凭据隔离

- HTTP 客户端绑定 `SessionLease`，不持有可无条件覆盖全局凭据的仓库。读、刷新替换和刷新失败撤销都验证同一租约。
- 登录开始/完成、登出及账号切换推进会话代次。旧请求不能改变新账号 Token、任务、基线或候选；已失效租约也不能提交任务/确认。
- 身份来自安全记录中的规范化服务地址与服务器 access JWT `sub`、规范用户名；解析 claims **仅用于本地隔离，不替代服务端鉴权**。
- 每个业务请求最多刷新一次、重试一次；并发 401 共用刷新，迟到旧 access 的 401 复用已完成刷新。
- 临时网络/服务错误不清空凭据；真正鉴权失败进入退出流程。匿名登录/远端登出本身不写凭据。
- 安全记录包含随机 session ID；`auth-session.json` **仅含该随机 ID 或 null**，不含 Token/用户名。
  登出先阻断恢复；系统条目删除失败但控制文件已提交时，重启仍不能恢复旧会话。
- 若安全存储和文件控制同时失败，会返回可见错误并撤销当前进程租约，**不承诺无法实现的持久退出保证**。
- 旧 token-only 开发记录需重新登录；不读取/迁移旧 Swift 凭据，也不回退明文。
- URL 拒绝凭据、查询参数、fragment；正式构建只接受 HTTPS，测试/开发才接受 loopback HTTP；HTTP 重定向关闭。

证据：`auth.rs::race_tests`、`creds.rs::tests`（控制文件真实 I/O，秘密条目注入）、`net/client.rs::tests`。

### 2.3 P3：统一仲裁与恢复

- Bootstrap、手动同步、未知结果恢复走同一仲裁。未归属/其他账号的重要本地数据，即使云端为空也必须明确确认。
- 无基线的双非空状态先展示候选，不盲 PUT；dirty 的空列表表示删除，不自动恢复云端任务。
- 远端版本变化形成候选；**未决时已确认基线保持原值**，不能把候选版本当成已批准覆盖的基线。
- PUT 前持久化不确定标记。响应丢失/崩溃后先 GET；发现云端变化时仲裁，不能再重放一次覆盖 PUT。
- 409 先保存“需要拉取候选”状态。即使随后 GET 失败或重启，也不能越过冲突直接 PUT。
- 本地/云端选择在同一核心提交中检查会话、归属、候选 UUID 与完整 i64 版本。再次冲突重新仲裁。
- 归属变化前保存独立 UUID 归档；归档失败阻断操作。恢复期间的新任务编辑转为候选；导入则使旧网络操作失效。
- 确认写入失败保留 dirty/标记；旧 PUT 成功只能确认它实际覆盖的任务编辑，不能清掉之后的修改。
- 自动上传关闭约束防抖、重试、重启与慢 GET 返回；手动同步/明确选择仍可执行。退避为 2 秒起、最多 60 秒。

证据：`src-tauri/src/engine/tests.rs` 的真实 reload、迟到 A→B 结果、空列表删除、无归属、首次 GET、
409 候选拉取失败、未知 PUT、归档/确认落盘失败、大整数与候选 UUID 用例。

### 2.4 P1/P2：IPC、前端握手与表单生命周期

- `CommandError` 包含机器码、文案、可重试性和可选字符串 `currentVersion`；内部文件内容/路径错误在命令边界收敛。
- Snapshot/Auth/Sync/Conflict 携带会话代次与事件修订；通知定位携带会话代次与不透明 notificationId；设置与迁移携带事件修订，设置还带已提交偏好修订。
- 所有订阅完成后才调用 `init_state`。启动响应和事件使用同一合并规则；旧会话、旧修订、已解决的旧候选不能回退 UI。
- 新会话状态不完整时重新读取；注册/握手失败可见并可重试。主题生命周期独立，不重新订阅；卸载清理迟到完成的订阅。
- 工作区按认证代次重建；漏掉登出事件但收到新账号时也不会留下旧草稿。形态切换不改变此 key。
- 版本 `9007199254740993` 以字符串显示/确认；Rust 解析规范十进制 i64，不经过 JS number。
- “稍后决定”仅隐藏当前候选，保留后台门禁；同步详情可重新打开。候选 ID 改变会清除旧选择。
- 修复固定浮窗/IME composition/快速回焦时误隐藏；window-shown 的监听注册纳入启动握手，通知不再依赖旧插件 JS listener。
- 设置窗口不再启动预载：按需创建、关闭销毁；主窗口拦截关闭并保留同一 WebView。

证据：`src/hooks/useDoing.test.tsx`、`src/lib/{snapshotMerge,contract,errors}.test.ts`、
`src/components/__tests__/interactions.test.tsx`，以及 Rust DTO 漂移与版本解析测试。
jsdom 焦点事件回放不能代替真实中文输入法、托盘、Spaces、DPI 或原生窗口内存验收。

### 2.5 P4：偏好与提醒

- `settings.json` 改为 schema 1 偏好封装，包含修订、设置和窗口坐标；写入/备份原子化，失败只发布错误、不发布新设置。
- 兼容读取旧**开发版**无版本设置与 `window.json`，不等同于完成旧 Swift 偏好迁移。损坏/未来版本不覆盖；读取失败关闭自动上传/通知并提示。
- 修改偏好、恢复默认、窗口坐标共用事务；空补丁不写盘，坐标不再通过截断 i64 转 i32 读取。
- 登录项设置失败不假装成功，操作后回读 OS 状态；实际安装位置、批准流程及旧 `SMAppService` 切换仍待验收。
- 提醒刷新串行化，提交前/后检查当前账号、会话、任务和截止日期。系统接受提交后才落盘记录；失败或期间改期/登出不消耗新资格。
- UUIDv4 路由在 OS 提交前原子落盘，同一账号/事项/截止时间重试复用 UUID；写记录失败允许同 ID 再提交。不声称跨 OS/磁盘严格 exactly-once。
- 点击先持久化 Pending 再唤起主面板；每次读取/ACK 校验会话、数据账号和迁移门禁。完成/改期但仍存在的任务仍定位，删除/未知/已消费的记录只打开工作区。
- 原生权限、跨重启定位、main-only ACL 和 UI 确认协议见 §2.9。**安装态真实授权/声音/热冷点击仍待验收。**

证据：`commands.rs::preference_transaction_tests`、`preferences.rs::tests`、`reminder/tests.rs`。

### 2.6 P3：真实 Go / MySQL（2026-09-11）

新增可复现启动器 `scripts/exercise-go-mysql.py`、测试专用 Go launcher `tests/go-mysql/main.go`
和默认忽略的 Rust 入口 `src-tauri/src/go_mysql_tests.rs`；说明见 [`tests/go-mysql/README.md`](../tests/go-mysql/README.md)。
此前运行汇总见 [`evidence/go-mysql-2026-09-11.json`](evidence/go-mysql-2026-09-11.json)。
P4 后又一次全新 fixture 的两阶段复跑见 [`evidence/go-mysql-2026-09-11-p4.json`](evidence/go-mysql-2026-09-11-p4.json)，两个阶段/清理均通过，原 Go 运行时摘要保持一致。

- 全新私有 MySQL 8.4.11 datadir，`--no-defaults --skip-networking --mysqlx=0`，仅私有 Unix socket；
  不安装系统服务，不连原配置/既有数据库。启动前核验真实 `@@datadir` 和关闭网络状态。
- 原 Go 运行时代码与三份 SQL migration 只读复制，**不复制 `internal/config`，不运行原 `cmd/server`**；
  临时 launcher 显式传测试 DB/JWT secret/3 秒 access TTL，仅监听随机回环端口。原 API/SQL 结构没有改动。
- 注册/规范用户名与稳定账号、空库版本 0、两个 AppState 同账号 bootstrap 与保存，
  中文/emoji/引号、排序/done/focus、带时区日期与毫秒精度均通过真实 SQL 往返。
- 真实 409 后保留确认基线/持久候选；暂缓、选择本地、再次冲突后选择云端与清历史通过。
- 等待真实 access 过期并确认 GET 401；两个并发业务请求共享一次成功刷新；旧 refresh 和登出后的 refresh 均为 401。
  不把 refresh 撤销夸大为无状态 access JWT 立即全部失效。
- 两账号使用相同 3 个 UUID：SQL 检查点为 **2 用户 / 6 行 / 3 个跨用户 UUID**，各读各的数据与独立版本。
  随后 dirty 空列表正确删除自己账号内容，另一账号保持不变。
- **实际退出并重启 MySQL、Go、Rust 测试进程**，保持相同 API origin；数据/偏好从磁盘加载后重新登录。
  原候选 UUID、确认基线 5、候选 6 与本地修改保留，自动/手动路径未绕过仲裁，明确选本地后才写入版本 7。
  再换号会归档原账号数据、形成 Ownership 仲裁，选云端后原账号仍保持独立。
- 测试证明/密码只在权限 0600 的临时文件或子进程环境里；报告没有 Token/密码/任务正文。
  Rust 客户端使用 **MemoryStore**，不触碰真实 Keychain/Windows 凭据或真实用户目录。
- 普通 Rust 测试不联网；裸 URL/缺失私有证明不能启动写测试。Python 离线守卫覆盖来源选择、排除 config、
  禁止 symlink/代理/重定向、私有报告和配置隔离。启动器检查精确入口确实执行，拒绝“匹配 0 项”的假绿灯。

本机 bottle 下载后已校验 SHA-256 `c772b71bb4f037a6a839c99f4fa842bdc638bc13abd7c277a34bd4f6b88f0846`，
未 `brew install`/注册服务。复用的原 Go 运行时源摘要为
`61a2c105493afc0c7d31e0bd2c9ee622294671b0026e805ceeae4f489b7ece33`（范围说明见测试 README）。
两阶段是正常进程重启，不是断电/SIGKILL、两台原生设备、系统凭据恢复或生产 HTTPS 验收。
两轮全新 fixture 复跑均通过。最终报告轮自动清理成功，六个记录的进程 PID 随后确认不存在；
开发时保留的合成数据库目录和上一轮未启动的预初始化 datadir 也已删除。
仅保留权限受限的已校验解包 MySQL 运行时（无运行中服务），便于后续重跑。

### 2.7 P4：回滚导出完整性与首次发布排他性

- 固定同一秒的回归测试先在旧实现上复现：第二次导出路径与第一次相同、覆盖已交付内容。
- 现在导出名称带 UUID，先序列化完整旧格式文档，再通过同目录暂存、内容刷新、no-clobber 发布。
  已有文件/目录/符号链接不能被覆盖，不把条目序列化失败降级为 `null`；只包含
  `items`、`focusID`、`notifiedDueIDs`，没有账号/同步候选/凭据/偏好。
- `write_new_atomic` 用同卷排他 hard link 发布完整暂存文件，之后移除自己的暂存名。
  文件系统不支持时如实失败，**不降级成直接写可见文件或覆盖旧文件**；常规替换保存仍使用原平台原子替换。
- 首次 `JsonRepo::initialize` 复用该原语，独立仓库实例即使各持不同锁也只有一个能初始化成功。
  回归检查 16 个独立写入方只有一个成功，最终文件完整；文件/符号链接冲突和导出权限失败保留旧内容与内存。
- ENOSPC 专用入口现在必须具有隔离小卷标记，不能因缺失环境而空跑为通过；同时检查替换保存和新建导出路径。
  卸载失败保留隔离目录，不再忽略卸载失败后继续删目录。

导出与首次提交的收尾仍不等于整个 §7.2 迁移完成；后续双文件事务实现与隔离验证见 §2.8，真实用户域/多屏/安装切换仍单独验收。

### 2.8 P4：双文件迁移事务、恢复 UI 与真实进程中断

协议详见 [`migration-transaction.md`](migration-transaction.md)；实现位于 `src-tauri/src/migrate/`。

- `migration-state.json` schema 1；每次事务独立 UUID。`source.json` 记录时间、来源、格式、SHA-256、大小、mtime 纳秒字符串及 Unix dev/inode。
  源任务、白名单偏好、原新版偏好（若存在）、暂存目标与冻结的几何信息分别保存，解析后重读并逐字段核对。
- Preparing → Ready → Publishing → Committed，另有 RollingBack / Cancelled / Rejected / KeptCurrent。
  发布前先持久化意图；错误记录只依据磁盘上的 durable phase，不能把内存中尚未成功写入的 Committed 顺手保存。
  重试不拼装新的来源快照；每次发布和完成标记前重新检查源内容、mtime/身份、偏好与旧进程。
- NSWorkspace 检查旧 bundle ID，`ps -axo pid=,comm=` 检查未打包 Swift 可执行文件；不读参数、不 kill 旧进程。
  macOS 通过 UserDefaults 的应用域读取偏好，**先筛白名单再序列化**，磁盘 plist 仅作额外 metadata 变化检测。
  白名单包括模式/外观、菜单栏焦点与逾期展示、提醒开关/声音/临期范围、自动同步和 panelOrigin；旧 Token、授权、登录项不进入备份。
- 启动恢复先于 data/settings 加载和凭据恢复；未完成/未知日志会保护 UI 权威、阻止任务/偏好/导出/认证写入并暂停自动同步和提醒。
  退出时不将受保护的空 UI 状态写回；下次依日志恢复。导入持久化独立的重新登录要求，新的认证后仍须 Ownership 仲裁。
- 撤销先核对两个目标均属于 before/after，再只撤销本事务仍拥有的写入；后续修改不覆盖。
  明确保留当前文件不会冒充导入完成。Committed 后不重放旧数据、不约束后续正常编辑。
- 调用层拒绝和终态重试保持只读，保留历史、偏好、当前会话和后台任务；是否加载已发布文件由 durable phase 决定，而不是返回值成功与否。
  新回归保留完整文件/核心状态/撤销重做/引擎代次/租约断言；“导入使旧恢复失效”明确断言 `sessionChanged`，没有宽泛忽略错误。
- UI 新增偏好选项、二次导入/撤销/保留确认、备份路径、错误/警告、Esc/焦点处理；设置窗口也保留恢复入口。
- 真实进程 kill 矩阵在 20 个准备/发布/完成标记/撤销检查点终止确切的私有子进程并 wait，Unix 验证 `SIGKILL`。
  准备阶段可新建重试、发布阶段同事务恢复、撤销阶段幂等继续；不用析构恢复或仅设置内存标记。
  来源变化、损坏/未知日志、备份/暂存校验、锁竞争、目标后续修改、鉴权隔离、调用层幂等均有夹具测试。
- 坐标依据 Cocoa 主屏顶边和完整位置/尺寸/scale 唯一匹配，修复 AppKit/Tao 名称不同导致恒定回退的问题。
  实际窗口恢复复用 `desktop::placement`：遍历所有工作区、使用目标屏缩放后的真实窗口尺寸；macOS 输出 logical position 绕开当前屏 scale 重解释，Windows 输出 physical position。
  纯函数覆盖 Retina、负坐标副屏、拔屏、混合 DPI 歧义、工作区边界和数值异常；旧偏好转换测试接通同一 selector/Position。

证据边界：原生 NSDictionary 测试仅操作内存合成对象；未读取真实旧 UserDefaults/任务/凭据，未启动旧 Swift 应用或执行正式切换。
本机位置实现/FFI 编译不等于真实多屏、NSPanel/Spaces、系统授权、旧 SMAppService 或 Windows 原生验收；菜单栏弹出仍是工作区内的安全 fallback，**尚非已验收的实际 tray rect 锚定**。

### 2.9 P4：原生通知、持久点击队列与 UI 确认

- 复核锁定桌面通知插件：权限固定 Granted、传入数字 ID 未用于投递、后台错误丢失。因此旧 dev 日志不能证明真实 OS 授权/接受；已移除 Rust/JS 通知插件和旧点击 IPC。
- 新 `doing-notifications` 薄适配器：macOS 使用真实 UNUserNotificationCenter/Settings 与 completion；Windows 使用 WinRT Setting/Show、转义 XML 和安装协议激活。
  macOS 拒绝借用 Terminal 身份；Objective-C 异常与 Rust callback panic 收敛为通用错误，不传播原生异常详情。测试仅抛内存合成对象。
- `notification-routes.json` schema 1：Reserved → Pending → Consumed；随机路由 UUID 关联事项/账号/截止时间，不含正文、用户名或 Token。
  文件锁 + 原子保存，拒绝 symlink/损坏/未知版本/重复 ID；8 MiB/4096 条上限，不驱逐活跃路由。存储失败不投递、不消费提醒资格。
- setup 在持久任务/认证恢复前安装原生 callback；callback 完成前同步落盘 Pending。主窗口就绪后拉取，未登录或归属不符时保留队列，不越权定位。
- `notification_next`/`notification_ack` 仅 main capability，ACK 带 sessionGeneration 再次鉴权；已完成/改期仍定位，删除/未知/旧 identifier 只开工作区，损坏文件保留并报错。
- Provider 等待 window-shown 注册；hook 丢弃迟到读取，ACK 去重/失败可重试，旧账号 ACK 不清新目标。Workspace 等实际任务行/焦点卡挂载、展开、选择并滚动后才 ACK。
- Windows NSIS/currentUser 仅声明开发 scheme；无运行时 `register_all()`、无正式协议抢占。核心及通知适配器 Windows cross-check 通过；完整 Tauri 壳仍因 Windows SDK/C 头文件缺失无法完成。
- 三个受保护子进程分别在 Reserved/Pending/Consumed 提交后被父进程 SIGKILL，重新从磁盘恢复；夹具目录设为 0700，保留原有私有目录守卫，不把子进程另加到常规测试数。

协议与验收清单：[`notification-delivery.md`](notification-delivery.md)；本轮证据：[`evidence/notifications-2026-09-11.md`](evidence/notifications-2026-09-11.md)。
这消除了纯内存路由缺口，但不证明真实 OS 横幅、声音、安装态冷启动或任何平台已原生验收。

## 3. 当前工作区完整验证

环境：macOS 27.0 arm64，Rust 1.98.1、Node 24.14.0、pnpm 11.24.0；真实服务联调使用 Go 1.26.1、MySQL 8.4.11。
这是开发环境，不是最低系统版本或已固定的发布工具链验收。迁移历史证据保留在 [`evidence/p4-migration-2026-09-11.md`](evidence/p4-migration-2026-09-11.md)，本轮通知重构验证见 [`evidence/notifications-2026-09-11.md`](evidence/notifications-2026-09-11.md)。

| 命令 | 实际结果与边界 |
| --- | --- |
| `cargo test --workspace --locked` | **206 项通过**：核心 55（44 单测 + 11 集成）、应用层 144、原生通知库 7。常规忽略磁盘满、类型生成、真实 Go/MySQL 入口及两个私有 kill helper；两个 kill 父测试计入 144，迁移 20 + 通知 3 个子进程不另加算 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，无 Rust warning |
| `pnpm run ci` | **102 项 Vitest 通过**（8 个文件）；TypeScript + Vite 构建通过。lint 保留 8 条 React 建议级 warning；旧 notification 静态/动态导入重复提示已消失 |
| `cargo test -p doing-desktop --lib regenerate_types_gen -- --ignored` | 本轮通知权限/读取/ACK DTO 已生成；再次生成内容 SHA-256 不变，漂移守卫通过 |
| `cargo build -p doing-desktop --release --locked` | 本轮 macOS arm64 release 桌面二进制编译通过（4m25s）；未启动、打包安装或进行原生验收 |
| `cargo check -p doing-core -p doing-notifications --target x86_64-pc-windows-msvc --locked` | 核心/Windows 文件替换和 WinRT 通知适配器编译通过；不覆盖 Tauri 壳或 Windows 运行行为 |
| `cargo check -p doing-desktop --target x86_64-pc-windows-msvc --locked` | **未通过**：ring 0.17.14 的 C 编译缺少 Windows SDK `assert.h`，exit 101；未伪造头文件或禁用 TLS 绕过 |
| `./scripts/exercise-disk-full.sh` | 1 MiB 临时卷上替换保存/新建导出均实际触发 ENOSPC；原主文件完整、无截断导出/自身暂存残留，卷与目录已确认清理 |
| `scripts/exercise-go-mysql.py --mysql-basedir … --server-source ../doing/server` | 全新 MySQL 两阶段真实联调通过；每阶段执行 1 次受保护 Rust 入口，实际重启 MySQL/Go/Rust；不计入上述 206 项普通测试；本轮报告见 `evidence/go-mysql-2026-09-11-notifications.json`，六个 owned PID 确认退出且临时数据清理 |
| `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py'` | 7 项离线启动器安全守卫通过，不访问数据库 |
| `git diff --check` | 通过 |

请使用 `pnpm run ci`；`pnpm ci` 是包管理器的内建安装命令，不是本项目验证脚本。
旧截图、性能采样和 `target/release/bundle` 里的旧 DMG 只是历史证据，**不能证明这次重写的新产物已完成原生/安装验收**。

## 4. 仍需继续完成的完整计划门禁

| 阶段 | 下一步与尚缺证据 |
| --- | --- |
| P0 高风险原型/基线 | Windows 11、macOS 14/Intel；实际托盘事件坐标与 DPI/工作区定位，NSPanel/Spaces/全屏等价性；真实 IME 与安装态通知；按懒加载后的产物重新测量并定版性能预算 |
| P1 核心/持久化/IPC | 旧 45 项行为的逐项映射仍未齐；所有公开命令/系统副作用的错误返回、加载失败界面、参数边界需继续审计；迁移之外的 kill/crash 与 Windows 原生故障矩阵仍需补齐 |
| P2 共用 UI | 当前逻辑用例通过不等于原生完成；新产物的设置开关/关闭重开/草稿/通知定位、真实中文 composition、文本撤销、混合 DPI/多屏和双端深浅色截图需验证与用户确认 |
| P3 认证/同步 | 新协调器已获隔离回归证据；本机实际 Go/MySQL 已通过 §2.6；仍需系统 Keychain/Windows 凭据失败路径、双端原生同账号离线/冲突/恢复/换号联调与正式配置审查 |
| P4 系统集成/迁移 | 迁移双文件事务/恢复 UI 与 20 检查点 SIGKILL、持久通知路由/ACK 与 3 个进程终止检查点已有隔离证据；旧用户域和混合 DPI 的原生语义、登录项状态/切换、真实通知权限/声音与安装态热冷点击仍需完成 |
| P5 发布 | 正式 API 地址（release 默认仍是不可用占位地址）、版本/源码身份、签名/公证、各架构 DMG/NSIS、WebView2、最小权限/日志/依赖/测试插件排除审计、干净机器安装升级卸载与交付清单 |
| P6 用户切换/旧版退役 | 未开始；仅在完整验收和用户明确批准后，备份真实数据、切换默认入口、停用旧登录项并退役 Swift 客户端 |

仍在推进完整计划，且还有可在工作区推进的事项；不能因为缺少 Windows/签名条件就把剩余工程工作当作已完成或全部阻塞。
正式完成仍须回到原计划 §11.2 逐项审计，本报告与单机测试不代替该检查表。
