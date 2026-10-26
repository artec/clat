use super::*;
impl ControlStorage {
    pub(crate) fn wechat_users(&self) -> serde_json::Value {
        im::users(&self.lock().im)
    }
    pub(crate) fn revoke_wechat_user(&self, id: &str) -> Result<(), ControlError> {
        self.commit(
            self.lock(),
            |state| self.save_im(state),
            |state| im::revoke_user(&mut state.im, id),
        )?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_user_revocation_removes_both_grants_and_survives_reopen() {
        let root = std::env::temp_dir().join(format!("clat-nav-wechat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        sentinel::initialize(&root).unwrap();
        let store = ControlStorage::open_ready(&root).unwrap();
        store
            .save_wechat_binding(
                &crate::im::ilink::Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        let challenge = store.create_wechat_pairing_code(10).unwrap();
        assert_eq!(
            store
                .attempt_wechat_pairing("grant-delivery", "test-user", &challenge.code, 20)
                .unwrap(),
            crate::im::PairingAttempt::Paired
        );
        store.set_wechat_allowed_user("test-user", true).unwrap();
        assert_eq!(store.wechat_users()[0]["paired"], true);
        assert_eq!(store.wechat_users()[0]["manual"], true);
        store.revoke_wechat_user("test-user").unwrap();
        assert!(!store.is_wechat_user_authorized("test-user"));
        drop(store);
        let reopened = ControlStorage::open_ready(&root).unwrap();
        assert!(reopened.wechat_users().as_array().unwrap().is_empty());
        drop(reopened);
        crate::test_support::cleanup_tree(&root);
    }
}
