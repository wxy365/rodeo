//! 附件文件本体的存储后端。两个后端共用同一套对象键
//! `{workspace_id}/{entry_code}/{id}_{safe_name}`：
//! - 本地后端把它当 `{data_dir}/attachments` 下的相对路径 —— 与历史版本磁盘布局完全一致，
//!   所以切到本后端时已有附件一个字都不用搬；
//! - RustFS 后端把它当桶内的对象键 —— 因此把 `{data_dir}/attachments` 的内容
//!   `rclone copy` / `mc mirror` 进桶即可完成迁移，不需要任何迁移代码。
//!
//! 两个后端都是 `Box<dyn ObjectStore>`：`object_store` 的 trait 本来就是为
//! 动态派发设计的（整表都是 `#[async_trait]`），共用一条代码路径比写两个分支更短。

use std::path::Path as FsPath;

use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{DynObjectStore, ObjectStore, PutPayload};
use tokio::io::AsyncReadExt;

use crate::config::{BlobBackend, StorageConfig};
use crate::error::AppError;

pub struct BlobStore {
    store: Box<DynObjectStore>,
    /// 启动探活只对远端后端有意义：本地后端在构造时已经确保根目录存在。
    kind: BlobBackend,
}

impl BlobStore {
    pub fn from_config(cfg: &StorageConfig) -> Result<Self, AppError> {
        let kind = cfg.blob.backend;
        let store: Box<DynObjectStore> = match kind {
            BlobBackend::Local => {
                let root = FsPath::new(&cfg.data_dir).join("attachments");
                // LocalFileSystem::new_with_prefix 内部会 canonicalize，目录不存在直接失败，
                // 所以必须先把根目录建出来（旧实现是按需 create_dir_all，这里提前到启动期）。
                std::fs::create_dir_all(&root).map_err(|e| {
                    AppError::Storage(format!("创建附件目录失败 {}: {e}", root.display()))
                })?;
                Box::new(
                    LocalFileSystem::new_with_prefix(&root)
                        .map_err(|e| AppError::Storage(format!("打开附件目录失败: {e}")))?,
                )
            }
            BlobBackend::Rustfs => {
                let b = &cfg.blob;
                if b.endpoint.trim().is_empty() || b.bucket.trim().is_empty() {
                    return Err(AppError::Storage(
                        "storage.blob.backend = \"rustfs\" 时必须配置 storage.blob.endpoint 与 bucket"
                            .to_string(),
                    ));
                }
                Box::new(
                    AmazonS3Builder::new()
                        .with_endpoint(b.endpoint.trim())
                        .with_allow_http(b.allow_http)
                        .with_region(b.region.trim())
                        .with_bucket_name(b.bucket.trim())
                        .with_access_key_id(b.access_key.trim())
                        .with_secret_access_key(b.secret_key.trim())
                        // RustFS / MinIO 的默认部署只认 path-style；虚拟主机风格需要 DNS 泛解析。
                        .with_virtual_hosted_style_request(false)
                        .build()
                        .map_err(|e| AppError::Storage(format!("初始化 RustFS 客户端失败: {e}")))?,
                )
            }
        };
        Ok(Self { store, kind })
    }

    /// 启动探活。端点不可达、桶不存在、密钥不对这三种错，如果拖到首次上传才暴露，
    /// 用户看到的只是一条含糊的上传失败；这里让它们在启动期就带着原因失败。
    pub async fn health_check(&self) -> Result<(), AppError> {
        if self.kind == BlobBackend::Local {
            return Ok(());
        }
        // 探针用 `list_with_delimiter` 而不是 `get`：object_store 把 HTTP 404 一律映射成
        // `Error::NotFound`（只看状态码，见 object_store-0.12.5/src/client/retry.rs:155），
        // 而「桶不存在」（NoSuchBucket）返回的也是 404。用 `get` 的话，
        // 「桶不存在」与「探针对象不存在」不可区分——后者是必然发生的，
        // 于是探活永远通过，恰好放行了最该拦下的那类配错。
        // 一次 ListObjects 的 404 只可能来自「桶不存在」，所以这里的 NotFound 必须当失败。
        match self
            .store
            .list_with_delimiter(Some(&Path::from("_healthcheck")))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(AppError::Storage(format!("附件对象存储不可用: {e}"))),
        }
    }

    /// 从 async-graphql 给的临时文件句柄整块读入后上传。
    /// 上限 50MB，一次性读进内存是可接受的取舍——换来两个后端共用同一条代码路径。
    /// 尺寸校验由调用方在读之前用 `metadata()` 完成，所以这里不会读到超限的内容。
    pub async fn put(&self, key: &str, content: std::fs::File) -> Result<(), AppError> {
        let mut src = tokio::fs::File::from_std(content);
        let mut buf = Vec::new();
        src.read_to_end(&mut buf)
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;
        self.store
            .put(&Path::from(key), PutPayload::from(buf))
            .await
            .map_err(|e| AppError::Storage(format!("写入附件失败 {key}: {e}")))?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, AppError> {
        let r = self
            .store
            .get(&Path::from(key))
            .await
            .map_err(|e| AppError::Storage(format!("读取附件失败 {key}: {e}")))?;
        let bytes = r
            .bytes()
            .await
            .map_err(|e| AppError::Storage(format!("读取附件失败 {key}: {e}")))?;
        Ok(bytes.to_vec())
    }

    pub async fn delete(&self, key: &str) -> Result<(), AppError> {
        self.store
            .delete(&Path::from(key))
            .await
            .map_err(|e| AppError::Storage(format!("删除附件失败 {key}: {e}")))
    }
}
