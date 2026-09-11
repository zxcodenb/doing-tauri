//! DTO → TypeScript 类型生成（计划 P1：Rust 为唯一定义源，禁止长期手写两份契约）。
//! 生成物：`src/types.gen.ts`（提交入库，前端构建不依赖 Rust 工具链）。
//! 漂移由 `types_gen_is_up_to_date` 拦截：任何 DTO 改动若未重新生成都会使 `cargo test` 失败。
//! 重新生成：`cargo test -p doing-desktop --lib regenerate_types_gen -- --ignored`

#[cfg(test)]
use ts_rs::{Config as TsConfig, TS};

/// 生成配置：输出到前端 `src/`，大整数（u64/i64）映射为 `number`（JSON 数值语义）。
#[cfg(test)]
fn config() -> TsConfig {
    TsConfig::new()
        .with_large_int("number")
        .with_out_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../src"))
}

/// 渲染整份 `types.gen.ts`：逐类型生成声明后按固定顺序拼接。
/// 不依赖 ts-rs 的同文件合并（其多类型合并对枚举存在整文件重建缺陷）。
#[cfg(test)]
fn render_all() -> String {
    let cfg = config();
    let parts: Vec<String> = vec![
        crate::error::CommandError::export_to_string(&cfg).expect("CommandError"),
        crate::reminder::NotificationPermission::export_to_string(&cfg)
            .expect("NotificationPermission"),
        crate::reminder::NotificationPermissionView::export_to_string(&cfg)
            .expect("NotificationPermissionView"),
        crate::events::ScrollTargetView::export_to_string(&cfg).expect("ScrollTargetView"),
        crate::events::ItemView::export_to_string(&cfg).expect("ItemView"),
        crate::events::SnapshotView::export_to_string(&cfg).expect("SnapshotView"),
        crate::events::SyncStateView::export_to_string(&cfg).expect("SyncStateView"),
        crate::events::SyncStateViewPayload::export_to_string(&cfg).expect("SyncStateViewPayload"),
        crate::events::SettingsView::export_to_string(&cfg).expect("SettingsView"),
        crate::events::AuthStateView::export_to_string(&cfg).expect("AuthStateView"),
        crate::events::ConflictView::export_to_string(&cfg).expect("ConflictView"),
        crate::migrate::MigrationStatus::export_to_string(&cfg).expect("MigrationStatus"),
        crate::commands::StartupView::export_to_string(&cfg).expect("StartupView"),
        crate::commands::MutationView::export_to_string(&cfg).expect("MutationView"),
        crate::commands::TextArg::export_to_string(&cfg).expect("TextArg"),
        crate::commands::DueArg::export_to_string(&cfg).expect("DueArg"),
        crate::commands::MoveArg::export_to_string(&cfg).expect("MoveArg"),
        crate::commands::AuthArg::export_to_string(&cfg).expect("AuthArg"),
        crate::commands::CloudChoiceArg::export_to_string(&cfg).expect("CloudChoiceArg"),
        crate::commands::SettingsPatch::export_to_string(&cfg).expect("SettingsPatch"),
        crate::commands::BoolArg::export_to_string(&cfg).expect("BoolArg"),
    ];
    // 每个串自带 ts-rs 的 NOTE 头（同文件依赖不产生 import 行）；以首个串的头为整份文件头。
    let (header, _) = parts[0]
        .split_once("\n\n")
        .expect("ts-rs 输出应含 NOTE 头与声明体");
    let mut out = String::from(header);
    out.push('\n');
    for part in &parts {
        let (_, decl) = part
            .split_once("\n\n")
            .expect("ts-rs 输出应含 NOTE 头与声明体");
        out.push('\n');
        out.push_str(decl);
    }
    // ts-rs 在带字段注释的声明中会生成行尾空格，统一规范化再写入/比较。
    let mut normalized = out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    normalized.push('\n');
    normalized
}

#[cfg(test)]
mod tests {
    const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/types.gen.ts");

    #[test]
    fn types_gen_is_up_to_date() {
        let expected = super::render_all();
        let actual = std::fs::read_to_string(PATH)
            .expect("缺少 src/types.gen.ts：先运行 regenerate_types_gen 生成");
        assert_eq!(
            actual, expected,
            "src/types.gen.ts 与 Rust DTO 定义不一致：运行 regenerate_types_gen 后重跑测试"
        );
        // serde-compat 守点：字段必须是 camelCase、枚举字面量必须是小写；
        // 且同文件依赖不得产生 import（否则 ts-rs 同文件判定失效）。
        for probe in [
            "focusId",
            "automaticSync",
            "cloudVersion",
            "legacyImportAvailable",
            "notifiedDueIds",
            "unauthorized",
        ] {
            assert!(
                expected.contains(probe),
                "生成物缺少 {probe}：serde rename 未生效？"
            );
        }
        assert!(
            expected.lines().all(|line| line == line.trim_end()),
            "生成物不能含行尾空格"
        );
        assert!(
            !expected.contains("import type {"),
            "同文件类型不应产生 import 语句"
        );
    }

    /// 显式重新生成（DTO 改动后先跑它，再跑测试确认无残留漂移）。
    #[test]
    #[ignore = "按需生成：cargo test -p doing-desktop --lib regenerate_types_gen -- --ignored"]
    fn regenerate_types_gen() {
        // 测试进程内顺序执行，写入即为最终内容。
        std::fs::write(PATH, super::render_all()).expect("写入 src/types.gen.ts");
    }
}
