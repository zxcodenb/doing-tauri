# 隔离的真实 Go / MySQL 契约联调

此入口用 **Rust 真实 AppState / CredentialVault / ApiClient / 同步协调器 / JSON 仓库**，
连接原工程 `doing/server` 的路由、handler、service、repo 和 migrations，再连接全新 MySQL 8.4。
它不是脚本 HTTP mock，也不是两台原生桌面/系统凭据/安装验收。

## 运行

先准备 Go（满足原 `go.mod`）、Rust/Tauri 的编译依赖、Python 3.10+ 和 MySQL 8.4 的 `bin/mysqld`。
不要求 MySQL 已经运行，不需要 `mysqladmin`、Docker、sudo 或系统服务。**不要用已有数据库替代 fixture。**

```sh
# 已安装 MySQL 8.4 时，例如：
python3 scripts/exercise-go-mysql.py \
  --mysql-basedir "$(brew --prefix mysql@8.4)" \
  --server-source ../doing/server \
  --report /tmp/doing-go-mysql-report.json

# 不访问网络/数据库的启动器安全守卫：
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

也可使用校验 SHA-256 后解包的官方 MySQL bottle；不必 `brew install`/`brew services start`。
未重定位的 bottle 如依赖额外库目录，可用 `--mysql-library-path '目录1:目录2:…'` 显式提供，
只传给 MySQL 子进程。启动器不会替用户安装软件或更改 Homebrew。Go 首次编译可能按原 `go.sum` 下载依赖。

普通 `cargo test --workspace --locked` 只运行不联网的 URL 守卫，真实联调入口默认 `#[ignore]`。
不要只加 `--ignored` 后手工指定现有 API：测试必须收到启动器创建的私有 manifest 并核验隔离服务证明，
缺失时在任何写请求之前失败。启动器还检查精确测试确实执行了 1 项，不把 Cargo 的“匹配 0 项”当成功。

## 隔离边界

- 每轮在短路径 `/tmp/doing-contract-*` 新建权限 **0700** 的目录；MySQL 独立 datadir/socket/pid/log。
- `mysqld --no-defaults --initialize-insecure` 只初始化新目录，服务用 `--skip-networking --mysqlx=0`，
  仅有该私有目录里的 Unix socket；无 TCP MySQL 端口。测试 root 无密码不用于任何现有实例。
- Go 启动器在建 schema 前通过 SQL 核验 `@@datadir` 是本轮目录、`@@skip_networking=1`。
- 原 Go 工程仅只读采集：复制所需源文件，**不复制 `internal/config`，不运行原 `cmd/server`**。
  原 main 的监听地址没有环境覆盖，因此测试专用 `main.go` 显式提供隔离 DB、随机 JWT secret、3 秒 access TTL，
  只监听 `127.0.0.1` 的随机端口；业务路由及三份 migration 原样复用。原 API/数据库结构没有改动。
- 随机测试密码、JWT secret、证明 nonce 不写报告/仓库；manifest 权限 0600，环境剔除既有 DB/Token/代理配置。
- HTTP 就绪证明先校验来源/随机 nonce，禁止重定向。测试专用 `/_doing_test/audit` 仅提供 SQL 汇总和请求计数，
  不返回 Token、密码哈希、用户名或事项内容；它只存在于临时 launcher，永不编译进桌面发布包或原 Go 服务。
- 客户端只使用 `MemoryStore`，没有读写 Keychain/Windows Credential Manager。所有任务/账号均为合成数据。
- 只持有并停止自己启动的子进程，不扫描/终止进程名，不使用 `pkill`、不注册开机服务、不启动旧 Swift 应用。
- 成功/失败/中断后都尝试收回子进程；默认清理临时文件。调试时可加 `--keep-artifacts` 保留**合成** fixture，
  仍会停止进程；不要把其中的 manifest/数据库上传到仓库。`--report` 是无凭据的汇总证据。

## 两个真实测试进程

| 阶段 | 断言 |
| --- | --- |
| `workflows` | 注册时空白裁剪、MySQL 不同大小写登录返回规范 JWT 用户名/稳定账号；空库基线 0 |
| | UUID/中文/emoji/引号、done、排序、focus、空截止时间、带偏移 RFC3339 和毫秒精度真实 SQL 往返 |
| | 两个独立 AppState 同账号：上传、bootstrap 恢复、真实 409、暂缓不等于解决、选择本地、再次冲突后选择云端/清历史 |
| | 等待真实 access 过期，确认原始 GET 401；两个并发业务请求只成功刷新一次；旧 refresh 被轮换撤销；登出后 refresh 被撤销 |
| | 两个账号使用相同的 3 个 UUID，SQL 汇总为 6 行/3 个跨账号 UUID，分别读到各自文本和版本 |
| | dirty 空列表作为删除上传，其他账号不受影响；保留 dirty/旧确认基线/候选 UUID 到真实文件，自动同步打开 |
| `process-restart` | **退出并重新启动 MySQL、Go 和 Rust 测试进程**，保留临时磁盘，API 地址不变；Go migration 仍为 `[1,2,3]` |
| | 新 AppState 从文件加载、用内存仓库重新登录；原候选 UUID/基线/本地修订仍在，自动/手动路径不绕过未决冲突 PUT |
| | 选择本地使用候选版本写入，确认数据库跨重启云端内容；换号先独立归档，再作 Ownership 仲裁，选择云端不污染原账号 |

服务端 access JWT 为无状态验证；登出撤销 **refresh**，不能据此声称所有已发 access 立即失效。
本测试使用正常退出/重启，不冒充断电、SIGKILL、未知 PUT 或跨进程系统凭据恢复测试。
后者的协调器故障/时序用例另见 `src-tauri/src/engine/tests.rs`，真实系统验收仍需补齐。

## 证据与剩余门禁

JSON 报告记录源摘要、实际工具版本、两个阶段进程 PID/退出结果和真实 SQL/HTTP 汇总；不包含测试秘密。
源摘要只覆盖复用的运行时代码/go.mod/go.sum/migrations（明确排除 config 和 Go 单测），不是整个仓库哈希。
相同命令可反复运行，每次新建数据库，既不依赖上轮状态，也不污染既有数据库。

这填补 P3 的本机实际 Go/MySQL 联调证据，**不代表整个 P3 或 P0–P6 完成**：
真实 Windows/macOS 安装态、OS 安全存储、生产 HTTPS/服务配置、真实设备离线、通知/IME/托盘与发布签名仍单独验收。
