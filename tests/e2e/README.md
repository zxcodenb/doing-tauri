# tests/e2e（P0 选型结论占位）

**历史 P0 初判（未完成双端原生验收，不能视为最终 E2E 选型）**：暂不接入 WebdriverIO Tauri E2E。

## 背景与约束

- 计划 §10.1/R7：WebdriverIO 的 Tauri service 走嵌入式 WebDriver；直接在 macOS 使用
  `tauri-driver` 的桌面支持存在限制（WKWebView 不受支持），官方友好路径集中在 Linux/Windows。
- 本机为 macOS arm64 单环境，且已实测：菜单栏应用的托盘按钮不接收 AX/合成鼠标事件
  （真实 NSEvent 才触发）；已有合成快捷键可送达应用窗口，但 Return/CJK 与真实候选窗仍未完成验收 —— 桌面 E2E 驱动在本机
  不能仅凭本机脚本覆盖计划里的全部原生交互面。

## 当前有限证据链（不能等同完整桌面验收）

1. Rust 层：同步引擎/迁移以「真实 TCP 脚本服务器 + 注入句柄」单测覆盖（见
   `src-tauri/src/engine/tests.rs`、`auth.rs`、`creds.rs`、`migrate.rs` 测试模块）。
2. UI 层：Testing Library + jsdom 的组件交互测试（`src/components/__tests__/`）。
3. 集成层：`scripts/dev-mock-server.py` + `DOING_SKIP_LOGIN` 联调（启动仲裁→冲突面板实测）。
4. 系统层：辅助功能读取（托盘菜单项/窗口几何/标题）+ 截图核对 + DMG 安装演练。

## 接入计划（当具备环境时）

- Windows runner/真机：`tauri-driver` + WebdriverIO service 驱动窗口内流程
  （登录→录入→完成→撤销→截止编辑→主题切换），托盘/IME/通知仍保留人工检查表。
- 复用 `scripts/` 下现有脚本作为夹具与断言辅助。


## 当前工作区的证据边界（2026-09-11）

Rust 已加入真实状态销毁/重载、并发 TCP 响应屏障、凭据控制文件与通知/偏好故障注入；
前端已加入异步订阅握手、旧会话/旧修订丢弃、候选 UUID 与 i64 字符串、失焦/composition 事件回放。
新增 `scripts/exercise-go-mysql.py`：隔离私有 MySQL + 原 Go 路由，真实两客户端协调器、409/刷新/账号 SQL 隔离，
并实际重启 MySQL/Go/Rust 测试进程后核验持久冲突。详见 [`../go-mysql/README.md`](../go-mysql/README.md)。
这仍然不是两台原生 UI/OS 安全存储验收。最新通过数量及命令见 `docs/migration-progress.md`。

这些测试和较早的开发截图/DMG 演练，都不证明本轮重写的新二进制已经通过真实输入法、托盘/Spaces、
通知横幅点击、懒加载设置开关/重开、Windows WebView2、双端同账号或实际安装包验收。

通知链路已替换为原生适配器 + 持久路由 + UI 定位后 ACK，覆盖跨重启/换号/完成/改期/删除和三个真实进程终止检查点。
旧通知插件的固定 Granted/异步 show 返回值不是 OS 投递证据。安装态权限、声音与热/冷点击的待验收清单见
[`../../docs/notification-delivery.md`](../../docs/notification-delivery.md)，本轮自动化结果见
[`../../docs/evidence/notifications-2026-09-11.md`](../../docs/evidence/notifications-2026-09-11.md)。
