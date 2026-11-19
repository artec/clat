use super::*;
use crate::control_storage::workspace::SessionFlags;

impl TrustedProjectApplication {
    /// INV-SL2：当前选择是否指向已归档会话。读控制存储的组织 flags
    /// （廉价、不扫会话列表）；无工作区注册时视为未归档。
    pub(super) fn selection_is_archived(&self, id: &SessionId) -> Result<bool, ApplicationError> {
        Ok(self
            .workspace_id
            .as_ref()
            .map(|workspace| {
                self.control
                    .session_flags(workspace)
                    .get(id.as_str())
                    .is_some_and(|flags| flags.archived)
            })
            .unwrap_or(false))
    }

    /// SL-C2（INV-SL1/4/5/6）：归档域两段式删除的 core 侧。活跃域零
    /// 删除路径——`session.delete` RPC 层拒绝，不靠 UI 隐藏。守卫顺序：
    /// 存在性 → 仅归档 → 定时规则（防御性重复检查，正常流到不了
    /// 「已归档且有规则」态）→ 打开中 → 共享引用；删除范围=会话整个
    /// 存储目录（journal + checkpoint + 投影缓存）+ 组织 flags，无孤儿。
    pub fn delete_session(&mut self, id: &str, confirmed: bool) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if !confirmed {
            return Err(ApplicationError::new(
                "explicit delete confirmation required",
            ));
        }
        let Some(summary) = self
            .list_sessions()?
            .iter()
            .find(|summary| summary.id.as_str() == id)
            .cloned()
        else {
            return Err(ApplicationError::new(
                "session does not exist in this project",
            ));
        };
        if !summary.archived {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "session.delete_not_archived",
            )));
        }
        if self.has_schedule_rules(Some(id))? {
            return Err(super::schedules::schedule_message("delete_blocked"));
        }
        if self
            .current_session_id()
            .is_some_and(|open| open.as_str() == id)
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "session.delete_open",
            )));
        }
        let references = self
            .history
            .as_ref()
            .map(|history| history.share_referencing_sessions(id, &crate::CancelToken::new()))
            .transpose()
            .map_err(|error| ApplicationError::presented(error.presentation()))?
            .unwrap_or_default();
        if !references.is_empty() {
            let titles = references
                .iter()
                .map(|(_, title)| title.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ApplicationError::presented(
                crate::i18n::UiMessage::new("session.delete_share_referenced")
                    .arg("titles", titles),
            ));
        }
        let key = self.session_key(&SessionId::new(id));
        self.sessions.remove_session(&key).map_err(session_error)?;
        // 会话每轮的原生文件恢复证据账本（file-review，项目级存储）：
        // 会话删除后不再可达，一并清除（工作区侧保留物不动）。
        if let Some(review) = self.project.file_review() {
            review
                .remove_session(id)
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        if let Some(workspace) = self.workspace_id.as_ref() {
            self.control
                .forget_session(workspace, id)
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        Ok(())
    }

    pub fn set_session_organization(
        &mut self,
        id: &str,
        pinned: Option<bool>,
        archived: Option<bool>,
        confirmed: bool,
    ) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if archived == Some(true) && self.has_schedule_rules(Some(id))? {
            return Err(super::schedules::schedule_message("archive_blocked"));
        }
        if archived == Some(true)
            && self
                .jobs
                .as_ref()
                .is_some_and(|jobs| jobs.has_session_work(id))
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "job.archive_blocked",
            )));
        }
        if archived == Some(true) && !confirmed {
            return Err(ApplicationError::new(
                "explicit archive confirmation required",
            ));
        }
        if !self.list_sessions()?.iter().any(|s| s.id.as_str() == id) {
            return Err(ApplicationError::new(
                "session does not exist in this project",
            ));
        }
        self.ensure_registered()?;
        let workspace = self
            .workspace_id
            .as_ref()
            .ok_or_else(|| ApplicationError::new("workspace unavailable"))?;
        let mut flags = self
            .control
            .session_flags(workspace)
            .get(id)
            .cloned()
            .unwrap_or_default();
        if let Some(value) = pinned {
            flags.pinned = value;
        }
        if let Some(value) = archived {
            flags.archived = value;
        }
        self.control
            .set_session_flags(workspace, id, flags)
            .map_err(|e| ApplicationError::new(e.to_string()))
    }

    pub(super) fn organize_summaries(&self, summaries: &mut [crate::SessionSummary]) {
        let flags = self
            .workspace_id
            .as_ref()
            .map(|id| self.control.session_flags(id))
            .unwrap_or_default();
        for summary in summaries.iter_mut() {
            let SessionFlags { pinned, archived } =
                flags.get(summary.id.as_str()).cloned().unwrap_or_default();
            summary.pinned = pinned;
            summary.archived = archived;
        }
        summaries.sort_by_key(|s| (!s.pinned, std::cmp::Reverse(s.last_activity_ms)));
    }
}
