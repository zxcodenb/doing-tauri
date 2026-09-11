//! Recoverable two-file publication. No ordinary writer may run while this journal is active.
use super::{legacy_preferences, problem, source::*};
use crate::error::CommandError;
use crate::preferences::PreferencesFile;
use doing_core::data::{parse_file, DataFile, FileParse};
use doing_core::repo::{write_atomic, write_new_atomic};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use uuid::Uuid;

type Result<T> = std::result::Result<T, CommandError>;
const JOURNAL_LIMIT: u64 = 128 * 1024;
const PREFS_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Preparing,
    Ready,
    Publishing,
    RollingBack,
    Committed,
    Cancelled,
    Rejected,
    KeptCurrent,
}
impl Phase {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Ready | Self::Publishing | Self::RollingBack
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    pub code: String,
    pub message: String,
}
impl From<&CommandError> for Failure {
    fn from(error: &CommandError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Summary {
    pub source_sha256: String,
    pub imported_preferences: bool,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    pub schema_version: u32,
    pub transaction_id: Uuid,
    pub source_path: PathBuf,
    pub import_preferences: bool,
    pub phase: Phase,
    pub requires_login: bool,
    pub summary: Option<Summary>,
    pub error: Option<Failure>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sources {
    schema_version: u32,
    transaction_id: Uuid,
    created_at: String,
    task: SourceProof,
    preferences: Option<PreferenceProof>,
    settings_before: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Prepared {
    schema_version: u32,
    transaction_id: Uuid,
    sources_sha256: String,
    data_sha256: String,
    settings_sha256: Option<String>,
    geometry: Option<legacy_preferences::Geometry>,
    summary: Summary,
}

/// Checkpoints are also the real process-kill seams; the production observer does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    JournalStarted,
    SourcesRecorded,
    TasksBackedUp,
    PreferencesBackedUp,
    BeforeSettingsBackedUp,
    DataStaged,
    SettingsStaged,
    PreparedRecorded,
    ReadyRecorded,
    LoginRequiredRecorded,
    CredentialsRevoked,
    PublishingRecorded,
    DataPublished,
    SettingsPublished,
    CompletionMarkerWillWrite,
    CompletedRecorded,
    RollbackRecorded,
    SettingsRolledBack,
    DataRolledBack,
    CancelledRecorded,
}
pub trait Observer: Send {
    fn reached(&mut self, step: Step) -> Result<()>;
}
impl Observer for () {
    fn reached(&mut self, _: Step) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct Coordinator {
    directory: PathBuf,
}
impl Coordinator {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn state_path(&self) -> PathBuf {
        self.directory.join("migration-state.json")
    }
    pub fn backup_path(&self, id: Uuid) -> PathBuf {
        self.directory.join("migrations").join(id.to_string())
    }
    fn data_path(&self) -> PathBuf {
        self.directory.join("data.json")
    }
    fn settings_path(&self) -> PathBuf {
        self.directory.join("settings.json")
    }
    fn artifact(&self, id: Uuid, name: &str) -> PathBuf {
        self.backup_path(id).join(name)
    }
    fn lock(&self) -> Result<File> {
        private_directory(&self.directory, false)?;
        let path = self.directory.join(".migration.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err(problem(
                "invalidMigrationFile",
                "迁移锁文件异常，已保留现场",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(path)
            .map_err(|_| problem("migrationIO", "备份旧文件失败：无法建立迁移锁，请检查权限"))?;
        lock.try_lock()
            .map_err(|_| problem("migrationBusy", "另一个迁移正在进行，或目录无法加锁"))?;
        Ok(lock)
    }
    pub fn read(&self) -> Result<Option<Journal>> {
        let Some(bytes) = read_regular(&self.state_path(), JOURNAL_LIMIT)? else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            problem(
                "invalidMigrationJournal",
                "迁移日志损坏，已暂停写入并保留现场",
            )
        })?;
        if value.get("schemaVersion").and_then(|v| v.as_u64()) != Some(1) {
            return Err(problem(
                "unsupportedMigrationJournal",
                "迁移日志版本无法识别，请使用对应版本，不能降级覆盖",
            ));
        }
        let journal: Journal = serde_json::from_value(value).map_err(|_| {
            problem(
                "invalidMigrationJournal",
                "迁移日志字段无效，已暂停写入并保留现场",
            )
        })?;
        if journal.transaction_id.is_nil()
            || !journal.source_path.is_absolute()
            || (matches!(
                journal.phase,
                Phase::Ready | Phase::Publishing | Phase::Committed
            ) && journal.summary.is_none())
            || journal
                .summary
                .as_ref()
                .is_some_and(|s| !valid_checksum(&s.source_sha256))
        {
            return Err(problem(
                "invalidMigrationJournal",
                "迁移日志关联信息无效，已保留现场",
            ));
        }
        Ok(Some(journal))
    }
    fn save(&self, journal: &Journal) -> Result<()> {
        json_atomic(&self.state_path(), journal, false)
    }
    fn record_error(&self, id: Uuid, error: &CommandError) {
        // The on-disk phase is authoritative. A failed save must not accidentally commit
        // an advanced in-memory phase while trying to record the error.
        if let Ok(Some(mut durable)) = self.read() {
            if durable.transaction_id == id && durable.phase.active() {
                if durable.phase == Phase::Preparing {
                    durable.phase = Phase::Rejected;
                }
                durable.error = Some(error.into());
                let _ = self.save(&durable);
            }
        }
    }

    fn expected(&self, id: Uuid) -> Result<Journal> {
        let journal = self
            .read()?
            .ok_or_else(|| problem("migrationChanged", "迁移记录已不存在，请重新读取状态"))?;
        if journal.transaction_id != id {
            return Err(problem("migrationChanged", "迁移记录已变化，旧操作已取消"));
        }
        Ok(journal)
    }
    pub fn begin(
        &self,
        source: &Path,
        import_preferences: bool,
        revision: u64,
        environment: &dyn Environment,
        authorize: &mut dyn FnMut() -> Result<()>,
        observer: &mut dyn Observer,
    ) -> Result<Journal> {
        let _lock = self.lock()?;
        let previous = self.read()?;
        if previous.as_ref().is_some_and(|j| j.phase.active()) {
            return Err(problem(
                "migrationPending",
                "上次导入尚未结束，请先恢复或撤销",
            ));
        }
        if let Some(existing) = read_regular(&self.data_path(), MAX_BYTES)? {
            if !matches!(parse_file(&existing), FileParse::Data(_)) {
                return Err(problem(
                    "invalidData",
                    "新版数据文件异常或版本未知，请先保全数据；不会导入覆盖",
                ));
            }
            return Err(problem(
                "targetInitialized",
                "新版已初始化，不会重复导入或覆盖已有内容",
            ));
        }
        environment.check_legacy_stopped()?;
        let source = read_source(source)?;
        let preferences = if import_preferences {
            Some(environment.preferences()?)
        } else {
            None
        };
        let settings_before = read_regular(&self.settings_path(), PREFS_LIMIT)?;
        let original_settings = settings_before
            .as_deref()
            .map(crate::preferences::parse)
            .transpose()?
            .unwrap_or_default();
        let estimated = (source.bytes.len() as u64)
            .checked_mul(5)
            .and_then(|n| n.checked_add(4 * PREFS_LIMIT + 1024 * 1024))
            .ok_or_else(|| problem("insufficientSpace", "导入大小超出支持范围"))?;
        if environment.free_space(&self.directory)? < estimated {
            return Err(problem(
                "insufficientSpace",
                "可用空间不足以同时保存来源、暂存目标和恢复记录，尚未导入",
            ));
        }
        let id = Uuid::new_v4();
        private_directory(&self.directory.join("migrations"), false)?;
        private_directory(&self.backup_path(id), true)?;
        let mut journal = Journal {
            schema_version: 1,
            transaction_id: id,
            source_path: source.proof.path.clone(),
            import_preferences,
            phase: Phase::Preparing,
            requires_login: previous.is_some_and(|j| j.requires_login),
            summary: None,
            error: None,
        };
        // Preparing never has permission to publish either target.
        self.save(&journal)?;
        observer.reached(Step::JournalStarted)?;
        let result = (|| {
            let sources = Sources {
                schema_version: 1,
                transaction_id: id,
                created_at: chrono::Utc::now().to_rfc3339(),
                task: source.proof.clone(),
                preferences: preferences.as_ref().map(|p| p.proof.clone()),
                settings_before: if import_preferences {
                    settings_before.as_deref().map(checksum)
                } else {
                    None
                },
            };
            json_atomic(&self.artifact(id, "source.json"), &sources, true)?;
            observer.reached(Step::SourcesRecorded)?;
            new_file(&self.artifact(id, "legacy-items.json"), &source.bytes)?;
            observer.reached(Step::TasksBackedUp)?;
            if let Some(preferences) = &preferences {
                new_file(
                    &self.artifact(id, "legacy-preferences.plist"),
                    &preferences.bytes,
                )?;
                observer.reached(Step::PreferencesBackedUp)?;
                if let Some(before) = &settings_before {
                    new_file(&self.artifact(id, "settings-before.json"), before)?;
                }
                observer.reached(Step::BeforeSettingsBackedUp)?;
            }
            let mut data = super::parse_legacy(&source.bytes).map_err(|_| {
                problem(
                    "invalidLegacyData",
                    "旧数据损坏、标识重复或日期无效；备份已保留，未发布目标",
                )
            })?;
            data.revision = revision;
            data.sync.dirty = !data.items.is_empty();
            data.sync.local_edit_revision = revision;
            let data_bytes = encode(&data)?;
            let normalized = checked_data(&data_bytes)?;
            if normalized != data {
                return Err(problem(
                    "migrationValidationFailed",
                    "旧数据包含当前格式无法无损保存的字段或时间精度，未发布目标",
                ));
            }
            new_file(&self.artifact(id, "data-staged.json"), &data_bytes)?;
            observer.reached(Step::DataStaged)?;
            let mut warnings = Vec::new();
            let geometry = preferences.as_ref().map(|_| environment.geometry());
            let settings = if let Some(preferences) = preferences {
                let converted = legacy_preferences::convert(
                    &preferences.bytes,
                    geometry.as_ref().expect("selected geometry"),
                )?;
                warnings = converted.warnings;
                let file = PreferencesFile {
                    revision: original_settings
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| problem("migrationValidationFailed", "偏好修订号已耗尽"))?,
                    settings: converted.settings,
                    window_origin: converted.origin,
                    ..PreferencesFile::default()
                };
                let bytes = encode(&file)?;
                if crate::preferences::parse(&bytes)? != file {
                    return Err(problem("migrationValidationFailed", "偏好暂存验证失败"));
                }
                new_file(&self.artifact(id, "settings-staged.json"), &bytes)?;
                observer.reached(Step::SettingsStaged)?;
                Some(bytes)
            } else {
                None
            };
            let summary = Summary {
                source_sha256: sources.task.sha256.clone(),
                imported_preferences: import_preferences,
                warnings,
            };
            let prepared = Prepared {
                schema_version: 1,
                transaction_id: id,
                sources_sha256: checksum(&read_required(
                    &self.artifact(id, "source.json"),
                    JOURNAL_LIMIT,
                )?),
                data_sha256: checksum(&data_bytes),
                settings_sha256: settings.as_deref().map(checksum),
                geometry,
                summary: summary.clone(),
            };
            json_atomic(&self.artifact(id, "prepared.json"), &prepared, true)?;
            observer.reached(Step::PreparedRecorded)?;
            // Re-read all staged targets/backups, parse them, and verify complete field/hash identity before publication.
            self.validated(&journal)?;
            journal.summary = Some(summary);
            journal.phase = Phase::Ready;
            self.save(&journal)?;
            observer.reached(Step::ReadyRecorded)?;
            self.publish(&mut journal, environment, authorize, observer)
        })();
        if let Err(error) = &result {
            self.record_error(id, error);
        }
        result?;
        Ok(journal)
    }
    fn validated(&self, journal: &Journal) -> Result<Validated> {
        let id = journal.transaction_id;
        require_directory(&self.backup_path(id))?;
        let prepared: Prepared = decode(&read_required(
            &self.artifact(id, "prepared.json"),
            JOURNAL_LIMIT,
        )?)?;
        let source_bytes = read_required(&self.artifact(id, "source.json"), JOURNAL_LIMIT)?;
        let sources: Sources = decode(&source_bytes)?;
        if prepared.schema_version != 1
            || sources.schema_version != 1
            || prepared.transaction_id != id
            || sources.transaction_id != id
            || prepared.sources_sha256 != checksum(&source_bytes)
            || prepared.summary.source_sha256 != sources.task.sha256
            || prepared.summary.imported_preferences != journal.import_preferences
            || sources.task.path != journal.source_path
            || sources.task.format != "doing-swift-v1-json"
            || sources.preferences.is_some() != journal.import_preferences
            || prepared.settings_sha256.is_some() != journal.import_preferences
            || prepared.geometry.is_some() != journal.import_preferences
        {
            return Err(problem(
                "migrationValidationFailed",
                "迁移暂存信息与日志不匹配，已保留现场",
            ));
        }
        let legacy = read_required(&self.artifact(id, "legacy-items.json"), MAX_BYTES)?;
        if checksum(&legacy) != sources.task.sha256
            || legacy.len() as u64 != sources.task.stamp.bytes
        {
            return Err(problem(
                "migrationValidationFailed",
                "旧任务备份校验失败，已阻止发布",
            ));
        }
        let data = read_required(&self.artifact(id, "data-staged.json"), MAX_BYTES)?;
        if checksum(&data) != prepared.data_sha256 {
            return Err(problem("migrationValidationFailed", "任务暂存校验失败"));
        }
        let parsed = checked_data(&data)?;
        let mut expected = super::parse_legacy(&legacy)
            .map_err(|_| problem("migrationValidationFailed", "旧任务备份格式异常"))?;
        expected.revision = parsed.revision;
        expected.sync.dirty = !expected.items.is_empty();
        expected.sync.local_edit_revision = parsed.revision;
        if parsed != expected {
            return Err(problem(
                "migrationValidationFailed",
                "任务数量、标识、顺序、时间、焦点或提醒记录核对失败",
            ));
        }
        let settings = if let Some(expected) = &prepared.settings_sha256 {
            let bytes = read_required(&self.artifact(id, "settings-staged.json"), PREFS_LIMIT)?;
            if checksum(&bytes) != *expected {
                return Err(problem("migrationValidationFailed", "偏好暂存校验失败"));
            }
            let parsed_settings = crate::preferences::parse(&bytes)?;
            let prefs = read_required(&self.artifact(id, "legacy-preferences.plist"), PREFS_LIMIT)?;
            if sources.preferences.as_ref().is_none_or(|p| {
                p.domain != legacy_preferences::DOMAIN || p.sha256 != checksum(&prefs)
            }) || legacy_preferences::selected_snapshot(&prefs)? != prefs
            {
                return Err(problem("migrationValidationFailed", "旧偏好备份校验失败"));
            }
            let converted = legacy_preferences::convert(
                &prefs,
                prepared.geometry.as_ref().expect("validated geometry"),
            )?;
            if parsed_settings.settings != converted.settings
                || parsed_settings.window_origin != converted.origin
            {
                return Err(problem(
                    "migrationValidationFailed",
                    "旧偏好逐字段转换核对失败",
                ));
            }
            Some(bytes)
        } else {
            None
        };
        let before = if let Some(expected) = &sources.settings_before {
            let bytes = read_required(&self.artifact(id, "settings-before.json"), PREFS_LIMIT)?;
            if checksum(&bytes) != *expected {
                return Err(problem(
                    "migrationValidationFailed",
                    "原新版偏好备份校验失败",
                ));
            }
            crate::preferences::parse(&bytes)?;
            Some(bytes)
        } else {
            None
        };
        if let Some(after) = &settings {
            let before_revision = before
                .as_deref()
                .map(crate::preferences::parse)
                .transpose()?
                .unwrap_or_default()
                .revision;
            if before_revision.checked_add(1) != Some(crate::preferences::parse(after)?.revision) {
                return Err(problem("migrationValidationFailed", "偏好迁移修订号不匹配"));
            }
        }
        Ok(Validated {
            sources,
            data,
            settings,
            before,
        })
    }
    fn unchanged_sources(
        &self,
        validated: &Validated,
        environment: &dyn Environment,
    ) -> Result<()> {
        environment.check_legacy_stopped()?;
        let actual = read_source(&validated.sources.task.path)?;
        if actual.proof != validated.sources.task {
            return Err(problem(
                "sourceChanged",
                "旧文件的内容、修改时间或标识已变化，导入已暂停；可撤销后重新导入",
            ));
        }
        if let Some(expected) = &validated.sources.preferences {
            if environment.preferences()?.proof != *expected {
                return Err(problem(
                    "sourceChanged",
                    "旧偏好在导入过程中变化，导入已暂停",
                ));
            }
        }
        Ok(())
    }
    fn publish(
        &self,
        journal: &mut Journal,
        environment: &dyn Environment,
        authorize: &mut dyn FnMut() -> Result<()>,
        observer: &mut dyn Observer,
    ) -> Result<()> {
        let validated = self.validated(journal)?;
        self.unchanged_sources(&validated, environment)?;
        // All targets must be either the exact before-state or this transaction's complete after-state.
        target_state(&self.data_path(), None, Some(&validated.data), MAX_BYTES)?;
        if let Some(after) = &validated.settings {
            target_state(
                &self.settings_path(),
                validated.before.as_deref(),
                Some(after),
                PREFS_LIMIT,
            )?;
        }
        journal.requires_login = true;
        journal.error = None;
        self.save(journal)?;
        observer.reached(Step::LoginRequiredRecorded)?;
        authorize()?;
        observer.reached(Step::CredentialsRevoked)?;
        journal.phase = Phase::Publishing;
        self.save(journal)?;
        observer.reached(Step::PublishingRecorded)?;
        self.unchanged_sources(&validated, environment)?;
        publish_target(&self.data_path(), None, &validated.data, MAX_BYTES)?;
        observer.reached(Step::DataPublished)?;
        self.unchanged_sources(&validated, environment)?;
        if let Some(after) = &validated.settings {
            publish_target(
                &self.settings_path(),
                validated.before.as_deref(),
                after,
                PREFS_LIMIT,
            )?;
        }
        observer.reached(Step::SettingsPublished)?;
        self.unchanged_sources(&validated, environment)?;
        if target_state(&self.data_path(), None, Some(&validated.data), MAX_BYTES)?
            != TargetState::After
            || validated.settings.as_ref().is_some_and(|after| {
                target_state(
                    &self.settings_path(),
                    validated.before.as_deref(),
                    Some(after),
                    PREFS_LIMIT,
                )
                .ok()
                    != Some(TargetState::After)
            })
        {
            return Err(problem(
                "targetChanged",
                "发布后的目标发生变化，已暂停完成标记，不会覆盖新内容",
            ));
        }
        journal.phase = Phase::Committed;
        observer.reached(Step::CompletionMarkerWillWrite)?;
        self.save(journal)?;
        observer.reached(Step::CompletedRecorded)?;
        Ok(())
    }
    pub fn resume(
        &self,
        id: Uuid,
        environment: &dyn Environment,
        authorize: &mut dyn FnMut() -> Result<()>,
        observer: &mut dyn Observer,
    ) -> Result<Journal> {
        let _lock = self.lock()?;
        let mut journal = self.expected(id)?;
        let result = match journal.phase {
            Phase::Preparing => {
                // No publishing intent exists, so no target belongs to this attempt. Retire partial preparation only.
                journal.phase = Phase::Rejected;
                journal.error = Some(Failure::from(&problem(
                    "migrationInterrupted",
                    "上次导入在暂存阶段中断，目标未发布；备份保留，可重新导入",
                )));
                self.save(&journal)
            }
            Phase::Ready | Phase::Publishing => {
                self.publish(&mut journal, environment, authorize, observer)
            }
            Phase::RollingBack => self.rollback(&mut journal, observer),
            _ => Ok(()), // A completed/cancelled journal never replays a snapshot over later edits.
        };
        if let Err(error) = &result {
            self.record_error(id, error);
        }
        result?;
        Ok(journal)
    }
    pub fn cancel(&self, id: Uuid, observer: &mut dyn Observer) -> Result<Journal> {
        let _lock = self.lock()?;
        let mut journal = self.expected(id)?;
        match journal.phase {
            Phase::Committed | Phase::KeptCurrent => {
                return Err(problem(
                    "migrationChanged",
                    "此迁移已结束，不能撤销之后的新工作",
                ))
            }
            Phase::Cancelled | Phase::Rejected => return Ok(journal),
            Phase::Preparing | Phase::Ready => {
                // Ready has not published; preserve even unrelated files created by somebody else.
                journal.phase = Phase::Cancelled;
                journal.error = None;
                self.save(&journal)?;
                observer.reached(Step::CancelledRecorded)?;
            }
            Phase::Publishing | Phase::RollingBack => {
                let result = self.rollback(&mut journal, observer);
                if let Err(error) = &result {
                    self.record_error(id, error);
                }
                result?;
            }
        }
        Ok(journal)
    }
    fn rollback(&self, journal: &mut Journal, observer: &mut dyn Observer) -> Result<()> {
        let validated = self.validated(journal)?;
        // Preflight BOTH targets before changing either one; refuse an unknown or later edit.
        target_state(&self.data_path(), None, Some(&validated.data), MAX_BYTES)?;
        if let Some(after) = &validated.settings {
            target_state(
                &self.settings_path(),
                validated.before.as_deref(),
                Some(after),
                PREFS_LIMIT,
            )?;
        }
        journal.phase = Phase::RollingBack;
        self.save(journal)?;
        observer.reached(Step::RollbackRecorded)?;
        if let Some(after) = &validated.settings {
            restore_before(
                &self.settings_path(),
                validated.before.as_deref(),
                after,
                PREFS_LIMIT,
            )?;
        }
        observer.reached(Step::SettingsRolledBack)?;
        restore_before(&self.data_path(), None, &validated.data, MAX_BYTES)?;
        observer.reached(Step::DataRolledBack)?;
        journal.phase = Phase::Cancelled;
        journal.error = None;
        self.save(journal)?;
        observer.reached(Step::CancelledRecorded)?;
        Ok(())
    }
    pub fn keep_current(&self, id: Uuid) -> Result<Journal> {
        let _lock = self.lock()?;
        let mut journal = self.expected(id)?;
        if !journal.phase.active() {
            return Ok(journal);
        }
        if let Some(data) = read_regular(&self.data_path(), MAX_BYTES)? {
            checked_data(&data)?;
        }
        if let Some(settings) = read_regular(&self.settings_path(), PREFS_LIMIT)? {
            crate::preferences::parse(&settings)?;
        }
        journal.phase = Phase::KeptCurrent;
        journal.requires_login = true;
        journal.error = Some(Failure::from(&problem(
            "migrationKeptCurrent",
            "已按确认保留当前新版文件；本次迁移未标记完成，旧来源和备份保持不变",
        )));
        self.save(&journal)?;
        Ok(journal)
    }
    pub fn acknowledge_new_login(&self) -> Result<()> {
        let _lock = self.lock()?;
        let Some(mut journal) = self.read()? else {
            return Ok(());
        };
        if journal.phase.active() {
            return Err(problem(
                "migrationPending",
                "请先完成、撤销或处理待恢复迁移",
            ));
        }
        if journal.requires_login {
            journal.requires_login = false;
            self.save(&journal)?;
        }
        Ok(())
    }
}

struct Validated {
    sources: Sources,
    data: Vec<u8>,
    settings: Option<Vec<u8>>,
    before: Option<Vec<u8>>,
}
#[derive(Debug, PartialEq, Eq)]
enum TargetState {
    Before,
    After,
}
fn target_state(
    path: &Path,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    max: u64,
) -> Result<TargetState> {
    let actual = read_regular(path, max)?;
    if actual.as_deref() == after {
        Ok(TargetState::After)
    } else if actual.as_deref() == before {
        Ok(TargetState::Before)
    } else {
        Err(problem(
            "targetChanged",
            "新版文件已被其他操作修改，已停止恢复；不会覆盖后续工作",
        ))
    }
}
fn publish_target(path: &Path, before: Option<&[u8]>, after: &[u8], max: u64) -> Result<()> {
    if target_state(path, before, Some(after), max)? == TargetState::After {
        return Ok(());
    }
    if before.is_some() {
        write_atomic(path, after)
    } else {
        write_new_atomic(path, after)
    }
    .map_err(|_| {
        problem(
            "migrationIO",
            "写入新版文件失败，日志和备份已保留，可恢复或撤销",
        )
    })
}
fn restore_before(path: &Path, before: Option<&[u8]>, after: &[u8], max: u64) -> Result<()> {
    if target_state(path, before, Some(after), max)? == TargetState::Before {
        return Ok(());
    }
    if let Some(bytes) = before {
        write_atomic(path, bytes)
            .map_err(|_| problem("migrationIO", "恢复原偏好失败，迁移仍受保护"))?;
    } else {
        fs::remove_file(path)
            .map_err(|_| problem("migrationIO", "撤销本次发布失败，迁移仍受保护"))?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().and_then(|p| File::open(p).ok()) {
            let _ = parent.sync_all();
        }
    }
    Ok(())
}
fn checked_data(bytes: &[u8]) -> Result<DataFile> {
    match parse_file(bytes) {
        FileParse::Data(data) => Ok(*data),
        _ => Err(problem(
            "invalidMigrationFile",
            "目标数据损坏或版本不受支持，已保留原文件",
        )),
    }
}
fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    write_new_atomic(path, bytes)
        .map_err(|_| problem("migrationIO", "备份或暂存写入失败，已有文件未被覆盖"))
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value)
        .map_err(|_| problem("migrationValidationFailed", "迁移文件序列化失败"))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|_| problem("migrationValidationFailed", "迁移备份或暂存清单格式无效"))
}
fn json_atomic(path: &Path, value: &impl Serialize, new: bool) -> Result<()> {
    let bytes = encode(value)?;
    if new {
        write_new_atomic(path, &bytes)
    } else {
        write_atomic(path, &bytes)
    }
    .map_err(|_| {
        problem(
            "migrationIO",
            "迁移日志写入失败，请检查权限和空间；已有来源和备份不变",
        )
    })
}
fn read_required(path: &Path, max: u64) -> Result<Vec<u8>> {
    read_regular(path, max)?.ok_or_else(|| {
        problem(
            "migrationValidationFailed",
            "迁移备份或暂存文件缺失，已阻止发布",
        )
    })
}
fn require_directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        return Err(problem(
            "invalidMigrationFile",
            "迁移目录缺失或是链接，已停止操作",
        ));
    }
    Ok(())
}
fn private_directory(path: &Path, new: bool) -> Result<()> {
    if !new && path.exists() {
        return require_directory(path);
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| problem("migrationIO", "备份旧文件失败：无法创建私有迁移目录"))
}
