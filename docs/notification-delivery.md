# 原生通知：投递、持久定位与确认协议

对应迁移计划 §6.2.4–7。实现位于 `crates/doing-notifications`、`src-tauri/src/reminder/` 和
`src/hooks/useNotificationNavigation.ts`。此文描述当前代码；真实横幅、声音、授权与安装态冷启动仍需原生验收。

## 1. 为什么不再使用旧通知插件

锁定的 `tauri-plugin-notification 2.4.0` 桌面实现不能提供本项目所需的证据边界：
权限查询/请求直接返回 Granted，桌面 builder 未将传入数字 ID 用于投递，后台 `show()` 错误没有返回给调用者。
因此旧的 `Granted` / `show() == Ok` 日志不证明 OS 已授权或接受；JS `onAction` 加进程内 map 也不能保证重启定位。
本轮移除 Rust/JS 通知插件，不借用 Terminal 的应用身份。

新 crate 只承担平台权限、提交和不透明点击标识，不包含任务仓库、凭据、Tauri 句柄或任意 URL 打开能力。
`submit() == Ok` 表示平台 API 接受请求，**不表示横幅已经显示或用户已经看到**。

## 2. 平台边界

### macOS

- setup 在加载任务/恢复认证前安装 `UNUserNotificationCenterDelegate`；不是等 WebView 就绪后才注册。
- 先读取本进程 bundle metadata，与配置的 application ID 严格匹配；裸二进制/不匹配返回 `BundleRequired`，不请求授权。
- 实际读取 `UNNotificationSettings.authorizationStatus`。启动不弹权限框，显式申请才请求 Alert/Sound。
- 等待权限请求 completion 最长 120 秒，查询/提交 completion 最长 10 秒；超时是不确定结果，不伪装成拒绝或成功。
- `UNNotificationRequest.identifier` 为 `doing-due-<UUID>`。仅默认点击进入定位，dismiss 不定位；未知旧 identifier 降级为只开工作区。
- Native 持有 delegate；delegate 只持有不可变的 Send+Sync Rust callback，callback 对 AppState 使用 Weak，避免引用环。
- Objective-C 调用与回调中的原生读取经过 `objc2::exception::catch`，外层再捕获 Rust panic，映射为通用不可用错误。
  不格式化/传播原生异常详情；Rust `catch_unwind` 单独不能捕获 `NSException`。此边界不承诺恢复 OOM、进程终止或任意内存错误。

### Windows

- 每次调用平衡初始化/释放 WinRT apartment；已存在的 STA 可继续使用，不释放不属于本次调用的 apartment。
- 读取 `ToastNotificationManager::CreateToastNotifierWithId(...).Setting()`，检查真正的 `Show()` 返回值。
- ToastGeneric XML 对任务文字转义并拒绝 XML 不允许的控制字符。
- 128 位 UUID 拆为 16 字符 Tag + 16 字符 Group，重试使用同一对值；URI 只带不透明 UUID。
- `activationType="protocol"`；由安装器声明的 `doing-dev-notification` 协议承接冷启动，single-instance 路径承接已运行实例。
  Tauri deep-link 初始 URL 和事件也进入同一 Rust 队列；不依赖临时进程内 Toast Activated listener。
- 字符串解析器只接受本 scheme、notification host 和规范的小写非 nil UUID；拒绝 query、fragment、额外路径与 percent escape。
  不将输入当作任意命令、文件路径或外部 URL 执行。
- `tauri.windows.conf.json` 使用 NSIS/currentUser，只声明开发协议；代码不调用运行时 `register_all()`。
  核心和原生适配器的 Windows 交叉检查不等于完整 Tauri 壳编译，更不等于协议已安装或冷启动点击已验收。

## 3. 持久文件与状态转换

文件：本应用数据目录下的 `notification-routes.json`，schemaVersion 为 1；与旧 Swift 数据目录隔离。
每条只包含：随机 UUIDv4 路由 ID、任务 UUID、账号归属（规范服务地址 + 稳定 account ID）、截止毫秒、phase。
不存任务正文、用户名或 Token；实际系统通知的正文仍按产品功能包含事项文本。

状态转换：

```text
到期且有资格 → reserve 落盘 Reserved → OS 接受提交 → 提交 data.json 中的已提醒记录
原生用户点击 → activate 同步落盘 Pending → 打开主工作区
登录/归属/迁移门禁通过 → notification_next（只读、不消费）
真实任务行或焦点卡挂载、选中并滚动 → notification_ack → 落盘 Consumed
```

- 同一账号/事项/截止时间的未消费记录复用 UUID，重试不生成新的定位别名；Consumed 后新提醒使用新 UUID。
- 文件锁串行化多个 Routes 实例；用同目录原子文件替换保存。Unix 使用 no-follow/0600，拒绝 symlink、损坏文件、未知版本和重复 ID。
- 文件上限 8 MiB、4096 条。满时只清理 Consumed，不驱逐未处理的活跃路由；无法预留时不调用 OS，也不消耗提醒资格。
- OS 与文件不是同一事务。崩溃可能发生在 OS 接受与本地记录保存之间；同 ID 重试只降低重复，不能承诺跨崩溃 exactly-once。
- 只有 OS 点击会把 Reserved 变为 Pending；单纯投递不能触发任务导航。
- 原生 callback 在完成前同步写入 Pending；WebView 尚未加载、尚未登录或应用再次重启都不依赖一次性的 JS 事件。
- Pending 只有完成 UI 定位后的 ACK 才被消费；文件失败不伪装成已确认。已删除任务无需生成虚假选择，可在读取队列时消费。
- 未知/已消费/旧 identifier 的有效点击只打开受门禁保护的工作区；损坏路由文件不覆盖、不猜任务，仍打开工作区并报告存储错误。

## 4. IPC、账号隔离与前端生命周期

| 命令 | 权限与语义 |
| --- | --- |
| `notification_permission` | 查询真实权限，返回 Granted / Denied / NotDetermined / Unavailable 及可读错误 |
| `notification_request_permission` | 显式申请；拒绝后的变更交给系统设置，再查询 |
| `notification_next` | 仅 main capability；当前有效会话、数据归属和迁移门禁都通过才返回对应 Pending |
| `notification_ack` | 仅 main capability；携带 notificationId + sessionGeneration，再次校验会话与数据归属后确认 |

设置窗口不能读取/确认任务定位。旧 `system_notify_clicked` 和旧插件 ACL 已移除；URI 中没有账号/任务 UUID/凭据。
通知路由不是访问凭证，不绕过登录、凭据租约或迁移保护。其他账号的 Pending 保留但不暴露给当前账号。
任务已完成或改期但仍存在时，仍按原计划定位；截止时间只参与提醒资格/路由预留，不是拒绝点击的理由。

Provider 将 window-shown 的监听器注册纳入 ready 握手。导航 hook 在启动、登录、窗口展示和权威快照变更时重新读取，
并用请求序号、挂载状态和会话代次丢弃迟到结果。ACK 按通知/会话去重，旧会话 ACK 的迟到成功不能清掉新账号目标。
Workspace 在展开已完成区、拿到实际 row ref 后再滚动/确认；焦点卡也有相同的定位 ref。失败保留可重试的持久 Pending。

## 5. 自动化证据与原生验收门禁

- Rust：跨状态重建/重启、投递/ACK/文件失败、同 ID 重试、并发实例、账号与迁移门禁、完成/改期/删除、未知路由降级。
- 真进程终止：三个受保护的私有子进程分别在 Reserved/Pending/Consumed 持久提交后停住；父进程仅终止自己启动的 child，
  wait 并断言 Unix SIGKILL，再用新 Routes 从磁盘验证恢复。测试夹具目录显式 0700，不放宽 helper 的安全守卫。
- 原生库测试只用内存合成异常、bundle mismatch 和 URI/XML 数据；不会向系统发送通知或触发授权。
- 前端：订阅握手、迟到读取/ACK、重试、完成区展开、焦点卡挂载后确认、权限显示及 main-only ACL。

最新命令与结果见 [`evidence/notifications-2026-09-11.md`](evidence/notifications-2026-09-11.md)。仍须在隔离测试身份的真实安装产物上验证：

1. 未决定/允许/拒绝/系统禁用后的真实状态和提示；未打包身份不得伪报已允许。
2. 横幅、通知中心、声音开关与 OS 声音设置；前台/后台点击及 dismiss。
3. 应用运行、已退出、尚未登录、登录恢复中、迁移恢复中各路径的热/冷启动点击。
4. 完成/改期/删除、旧账号/换号、多个 Pending、旧通知或路由无法读取时的安全行为。
5. macOS 最低版本/Intel、Windows 11 安装/升级/卸载及当前用户协议注册，不抢占正式版身份。

这些步骤尚未验收；不得据单元测试、进程 kill 或交叉编译宣布 P4/P5/P6 完成。
