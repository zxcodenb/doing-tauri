# tests/e2e（P0 选型结论占位）

**当前结论（P0 初判，待有 Windows/CI 双端环境后复核）**：暂不接入 WebdriverIO Tauri E2E。

## 背景与约束

- 计划 §10.1/R7：WebdriverIO 的 Tauri service 走嵌入式 WebDriver；直接在 macOS 使用
  `tauri-driver` 的桌面支持存在限制（WKWebView 不受支持），官方友好路径集中在 Linux/Windows。
- 本机为 macOS arm64 单环境，且已实测：菜单栏应用的托盘按钮不接收 AX/合成鼠标事件
  （真实 NSEvent 才触发），系统级合成键盘事件亦不可注入 —— 桌面 E2E 驱动在本机
  无法覆盖计划里最关键的交互面。

## 当前替代证据链（等同覆盖范围）

1. Rust 层：同步引擎/迁移以「真实 TCP 脚本服务器 + 注入句柄」单测覆盖（见
   `src-tauri/src/engine.rs`、`migrate.rs` 测试模块）。
2. UI 层：Testing Library + jsdom 的组件交互测试（`src/components/__tests__/`）。
3. 集成层：`scripts/dev-mock-server.py` + `DOING_SKIP_LOGIN` 联调（启动仲裁→冲突面板实测）。
4. 系统层：辅助功能读取（托盘菜单项/窗口几何/标题）+ 截图核对 + DMG 安装演练。

## 接入计划（当具备环境时）

- Windows runner/真机：`tauri-driver` + WebdriverIO service 驱动窗口内流程
  （登录→录入→完成→撤销→截止编辑→主题切换），托盘/IME/通知仍保留人工检查表。
- 复用 `scripts/` 下现有脚本作为夹具与断言辅助。
