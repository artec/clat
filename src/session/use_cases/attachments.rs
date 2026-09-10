//! attachments use cases, internal to SessionService.
use super::*;

impl SessionService {
    /// 导入图片附件（M4，2026-08-19）：先整体校验（存在、扩展名合法、
    /// ≤8MiB），再复制进会话目录的 `attachments/` 子目录（uuid 文件名
    /// 保留原扩展名）。返回 (绝对路径, MIME) 列表——绝对引用随后进
    /// journal，回放零换算；原件此后可删可改，会话自包含。
    /// 校验失败在任何复制之前返回错误（不留半套附件）。
    /// 导入图片附件（M4 → MM-1A 元数据化 → MM-1 S2/S3 接线 store）：
    /// [`crate::session::attachments::AttachmentStore::admit`] 完成
    /// S1 校验 + 批次上限 + 完整解码规范化 + 内容寻址发布——返回
    /// [`JournalImage`]：attachmentId = 规范化字节的 sha256（opaque）、
    /// 宽高 = 规范化后值（original_* 记源尺寸）、字节 = 规范化计数，
    /// 全部随 journal 落盘（回放重建 descriptor 零文件 I/O）。批次
    /// 失败在任何 journal 写入之前整体失败（零可达半成品，INV-MM1-3）。
    pub(crate) fn import_attachments(
        &self,
        sources: &[std::path::PathBuf],
    ) -> Result<Vec<crate::message::JournalImage>, SessionError> {
        if self.is_read_only() {
            return Err(SessionError::UnsupportedFormat(
                "legacy session is read-only; use /update first".into(),
            ));
        }
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let (key, attachments_dir) = {
            let active = self.active.lock().expect("active");
            let session = active
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
            (
                session.key.clone(),
                crate::session::path_layout::session_dir(
                    self.backend.root_path(),
                    session.key.project.header_cwd.as_deref(),
                    &session.key.id,
                )
                .join("attachments"),
            )
        };
        let session_dir = self.backend.create_session_dir(&key)?;
        let store = crate::session::attachments::AttachmentStore::open_in_session(
            &session_dir,
            attachments_dir,
        )
        .map_err(|error| SessionError::Io(format!("open attachment store: {error}")))?;
        let stored = store
            .admit(sources)
            .map_err(|error| SessionError::Io(error.to_string()))?;
        Ok(stored
            .into_iter()
            .map(|stored| crate::message::JournalImage {
                descriptor: crate::message::AttachmentDescriptor {
                    attachment_id: stored.id,
                    media_type: stored.media_type.to_owned(),
                    width: stored.width,
                    height: stored.height,
                    bytes: stored.bytes,
                    display_name: stored.display_name,
                    original_width: Some(stored.original_width),
                    original_height: Some(stored.original_height),
                },
                path: stored.blob_path,
            })
            .collect())
    }

    /// MM-2/W5 byte admission for sources already read through a narrower
    /// capability (project-relative no-follow reads or core-minted run
    /// scratch). It deliberately reuses AttachmentStore normalization and
    /// content-addressed publication; callers never get to mint an arbitrary
    /// descriptor/path pair.
    pub(crate) fn import_attachment_bytes(
        &self,
        bytes: &[u8],
        display_name: &str,
    ) -> Result<crate::message::JournalImage, SessionError> {
        let store = self.active_attachment_store()?;
        let stored = store
            .admit_bytes(bytes, display_name)
            .map_err(|error| SessionError::Io(error.to_string()))?;
        Ok(journal_image(stored))
    }

    /// Resolve an attachment id only when it is reachable from the active
    /// model surface. An orphan blob whose digest is guessed is not authority.
    /// The returned path has already crossed the session fence; provider reads
    /// still use their final no-follow open to close replacement races.
    pub(crate) fn resolve_active_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<crate::message::JournalImage, SessionError> {
        if attachment_id.is_empty()
            || attachment_id.len() > 128
            || !attachment_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(SessionError::NotFound("invalid attachment id".into()));
        }
        for (_, item) in self.surface_nodes()? {
            match item {
                ModelItem::User { content } | ModelItem::Assistant { content, .. } => {
                    for part in content {
                        if let crate::model::ContentPart::Image { path, media_type } = part
                            && attachment_path_matches(&path, attachment_id)
                        {
                            return journal_image_from_path(
                                attachment_id,
                                &path,
                                &media_type,
                                None,
                            );
                        }
                    }
                }
                ModelItem::ToolResult(result) => {
                    let image_blocks = result.blocks.iter().filter_map(|block| match block {
                        crate::message::ContentBlock::Image { attachment } => Some(attachment),
                        crate::message::ContentBlock::Text { .. } => None,
                    });
                    let image_parts = result.image_parts.iter().filter_map(|part| match part {
                        crate::model::ContentPart::Image { path, .. } => Some(path),
                        crate::model::ContentPart::Text(_) => None,
                    });
                    if image_blocks.clone().count() != image_parts.clone().count() {
                        // Descriptor authority and fenced paths are parallel
                        // projections of one ordered durable content array.
                        // A cardinality drift means their pairing is no longer
                        // provable, so fail closed for the whole tool result.
                        continue;
                    }
                    for (attachment, path) in image_blocks.zip(image_parts) {
                        if attachment.attachment_id == attachment_id {
                            return Ok(crate::message::JournalImage {
                                descriptor: attachment.clone(),
                                path: path.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Err(SessionError::NotFound(format!(
            "attachment `{attachment_id}` is not reachable from the active session"
        )))
    }

    /// 读取当前会话中**可达**的图片内容。先走同一 reachability 判定，
    /// 再以 no-follow 打开并生成有界、摘要已验证的不可变快照，避免
    /// guessed blob id、软链、前端传入路径或 verify→stream 原位改写成为
    /// 读取权限。返回的 reader 不带路径，调用方只能以受限块大小消费它。
    pub(crate) fn open_active_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<ActiveAttachmentReader, SessionError> {
        let image = self.resolve_active_attachment(attachment_id)?;
        let content_addressed = image.descriptor.attachment_id.len() == 64
            && image
                .descriptor
                .attachment_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        let (snapshot, bytes) = if content_addressed {
            let key = self
                .active
                .lock()
                .expect("active")
                .as_ref()
                .ok_or_else(|| SessionError::NotFound("no active session".into()))?
                .key
                .clone();
            let dir = self.backend.open_session_dir(&key)?;
            crate::session::attachments::AttachmentStore::read_session_blob(
                &dir,
                &image.descriptor.attachment_id,
            )
            .map_err(|error| {
                SessionError::NotFound(format!("attachment integrity verification failed: {error}"))
            })?
        } else {
            open_attachment_file(&image.path)?
        };
        if !crate::media::media_type_matches_bytes(&image.descriptor.media_type, &snapshot) {
            return Err(SessionError::NotFound(
                "attachment media type verification failed: attachment media type does not match its bytes".into(),
            ));
        }
        // New durable descriptors are minted from normalized content and must
        // describe those exact bytes. Legacy ids may legitimately carry
        // bytes=0 (unknown), but a content-addressed descriptor has no such
        // compatibility allowance: bind its count to the same verified file
        // handle before any frontend can consume it.
        if content_addressed && image.descriptor.bytes != bytes {
            return Err(SessionError::NotFound(
                "attachment descriptor byte count does not match its bytes".into(),
            ));
        }
        if content_addressed {
            let expected = (image.descriptor.width, image.descriptor.height);
            if crate::media::image_dimensions_bytes(&snapshot) != Some(expected) {
                return Err(SessionError::NotFound(
                    "attachment dimension verification failed: attachment dimensions do not match its bytes".into(),
                ));
            }
        }
        Ok(ActiveAttachmentReader {
            descriptor: image.descriptor,
            bytes,
            file: std::io::Cursor::new(snapshot),
        })
    }

    fn active_attachment_store(
        &self,
    ) -> Result<crate::session::attachments::AttachmentStore, SessionError> {
        if self.is_read_only() {
            return Err(SessionError::UnsupportedFormat(
                "legacy session is read-only; use /update first".into(),
            ));
        }
        let active = self.active.lock().expect("active");
        let session = active
            .as_ref()
            .ok_or_else(|| SessionError::NotFound("no active session".into()))?;
        let key = session.key.clone();
        let attachments_dir = crate::session::path_layout::session_dir(
            self.backend.root_path(),
            session.key.project.header_cwd.as_deref(),
            &session.key.id,
        )
        .join("attachments");
        drop(active);
        let session_dir = self.backend.create_session_dir(&key)?;
        crate::session::attachments::AttachmentStore::open_in_session(&session_dir, attachments_dir)
            .map_err(|error| SessionError::Io(format!("open attachment store: {error}")))
    }
}
