//! Doing 业务核心：任务与历史、截止规则、设置与原子持久化。
//! 本 crate 不依赖任何平台（无 AppKit / Win32 / WebView），可独立测试。

pub mod clock;
pub mod data;
pub mod due;
pub mod error;
pub mod item;
pub mod repo;
pub mod settings;
pub mod store;

pub use clock::Clock;
pub use data::{DataFile, SyncMeta};
pub use due::DueState;
pub use error::{CoreError, Result};
pub use item::Item;
pub use settings::{AppAppearance, AppSettings, SettingsChange};
pub use store::{Mutation, Store};
