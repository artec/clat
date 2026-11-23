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
    /// 「已归档且有规则」态）→ 共享引用；删除范围=会话整个存储目录
    /// （journal + checkpoint + 投影缓存）+ 恢复账本 + 组织 flags，无孤儿。
    ///
    /// 目标是当前打开（浏览中）的归档会话时**不拒绝**（2026-11-19
    /// 负责人驳回"打开中拒绝"设计——删除入口就在归档行菜单，浏览态
    /// 删除是正常路径）：先按 /new 语义腾空选择（quiesce writer + 持久
    /// 化 None，两阶段），再整删目录；调用方据此跳转全新会话。
    /// 返回值 = 是否发生了该腾空跳转。
    ///
    /// INV-SL7：三个存储依次遗忘且每步幂等，停在存储之间的删除可以续做
    /// （见 [`Self::finish_interrupted_delete`]）。INV-SL8：选择腾空之后
    /// 才有存储失败时，错误带上「选择已变更」——Host 已经切换，各客户端
    /// 据此重同步，即使本次调用报告的是失败。
    pub fn delete_session(&mut self, id: &str, confirmed: bool) -> Result<bool, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if !confirmed {
            return Err(ApplicationError::new(
                "explicit delete confirmation required",
            ));
        }
        let sessions = self.list_sessions()?;
        let Some(summary) = sessions.iter().find(|summary| summary.id.as_str() == id) else {
            return self.finish_interrupted_delete(id);
        };
        if !summary.archived {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "session.delete_not_archived",
            )));
        }
        if self.has_schedule_rules(Some(id))? {
            return Err(super::schedules::schedule_message("delete_blocked"));
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
        self.commit_session_delete(id)
    }

    /// INV-SL7：组织索引仍标为已归档且会话目录确认已不存在时，续做剩余
    /// 清理。目录存在或检查失败时，归档标记本身不能授权物理删除。
    pub(super) fn finish_interrupted_delete(&mut self, id: &str) -> Result<bool, ApplicationError> {
        let session = SessionId::new(id);
        if !self.selection_is_archived(&session)?
            || !self
                .sessions
                .confirm_session_directory_removed(&self.session_key(&session))
                .map_err(session_error)?
        {
            return Err(ApplicationError::new(
                "session does not exist in this project",
            ));
        }
        self.forget_session_metadata(id)?;
        Ok(false)
    }

    /// 删除的提交段：目标仍是当前打开的会话时先按 /new 语义腾空选择
    /// （quiesce writer + 持久化 None，两阶段），再让三个存储依次遗忘它。
    /// 每一步都幂等，任何一步失败后重试从停下的地方继续。选择已经腾空
    /// 而后续步骤失败时，错误带上「选择已变更」（INV-SL8）——Host 内部
    /// 已经切换，各客户端必须据此重同步，即使本次调用报告的是失败。
    fn commit_session_delete(&mut self, id: &str) -> Result<bool, ApplicationError> {
        let jumped_to_new = self
            .current_session_id()
            .is_some_and(|open| open.as_str() == id);
        if jumped_to_new {
            self.new_session()?;
        }
        self.forget_session_everywhere(id).map_err(|error| {
            if jumped_to_new {
                error.with_selection_changed()
            } else {
                error
            }
        })?;
        Ok(jumped_to_new)
    }

    /// 让三个存储遗忘一个会话，顺序固定且每步幂等（已不存在视为已删）：
    /// 会话整个存储目录（journal + checkpoint + 投影缓存）→ 每轮的原生
    /// 文件恢复证据账本（file-review，项目级存储；工作区侧保留物不动）→
    /// 组织 flags（归档/置顶 + sessionIds 成员索引）。无孤儿。
    fn forget_session_everywhere(&self, id: &str) -> Result<(), ApplicationError> {
        let key = self.session_key(&SessionId::new(id));
        self.sessions.remove_session(&key).map_err(session_error)?;
        self.forget_session_metadata(id)
    }

    fn forget_session_metadata(&self, id: &str) -> Result<(), ApplicationError> {
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

#[cfg(test)]
impl TrustedProjectApplication {
    /// Test seam for the delete chain: the next organization-index write for
    /// this session fails, the way a full disk does between the delete's
    /// stores. The serve RPC tests reach it through the application because
    /// they cannot see the private storage handle.
    pub(crate) fn inject_next_session_forget_failure(&self, id: &str) {
        self.control.inject_next_forget_session_failure(id);
    }
}
