use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Entry 附件。元数据进 RocksDB，文件本体落 `{data_dir}/attachments`。
///
/// 存储路径**不进实体**：由 workspace_id / entry_code / id / filename 现算，
/// 这样路径与文件名不会各自漂移。`filename` 是上传时的原始名，只用于展示与
/// 下载响应头；磁盘上的文件名是净化后的版本（见 `service::attachment::safe_name`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: Ulid,
    pub entry_code: String,
    /// 冗余存一份：权限校验与「这个附件属于哪个空间」不必每次回查 Entry。
    pub workspace_id: Ulid,
    pub filename: String,
    /// 客户端上报的 MIME，只当提示看：下载响应绝不回声此值。
    pub content_type: String,
    pub size: u64,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
}

impl Attachment {
    pub fn new(
        entry_code: String,
        workspace_id: Ulid,
        filename: String,
        content_type: String,
        size: u64,
        actor: Ulid,
    ) -> Self {
        Self {
            id: Ulid::new(),
            entry_code,
            workspace_id,
            filename,
            content_type,
            size,
            created_by: actor,
            created_at: Utc::now(),
        }
    }
}
