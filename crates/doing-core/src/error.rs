use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("任务不存在")]
    NotFound,
    #[error("操作未生效：内容为空或没有变化")]
    NoOp,
    #[error("不能对已完成任务设置焦点")]
    InvalidFocus,
    #[error("数据文件损坏或格式不受支持: {0}")]
    Corrupt(String),
    #[error("数据文件版本过高: {0}")]
    UnknownSchema(serde_json::Value),
    #[error("文件读写失败: {0}")]
    Io(String),
    #[error("序列化失败: {0}")]
    Encode(String),
    #[error("设置值超出范围")]
    OutOfRange,
}
