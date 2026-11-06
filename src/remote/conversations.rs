use super::{RemoteChannel, RemoteChatBinding, RemoteChatId, RemoteUserId};
use std::collections::HashMap;
use std::sync::Mutex;

/// Process-local state only: it must never become a new persistence protocol.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RemoteRoute {
    pub(crate) channel: RemoteChannel,
    pub(crate) user: RemoteUserId,
    pub(crate) chat: RemoteChatId,
}

#[derive(Default)]
struct Conversation {
    armed: bool,
    fallback: Option<RemoteChatBinding>,
}

#[derive(Default)]
pub(crate) struct RemoteConversations {
    routes: Mutex<HashMap<RemoteRoute, Conversation>>,
}

impl RemoteConversations {
    pub(crate) fn arm(&self, route: RemoteRoute) -> Result<(), String> {
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| "remote conversation state poisoned")?;
        if routes.len() >= 128 && !routes.contains_key(&route) {
            let old = routes
                .iter()
                .find(|(_, state)| state.fallback.is_none())
                .map(|(key, _)| key.clone())
                .ok_or("remote conversation capacity exhausted")?;
            routes.remove(&old);
        }
        routes.insert(
            route,
            Conversation {
                armed: true,
                fallback: None,
            },
        );
        Ok(())
    }

    pub(crate) fn snapshot(
        &self,
        route: &RemoteRoute,
    ) -> Result<(bool, Option<RemoteChatBinding>), String> {
        let routes = self
            .routes
            .lock()
            .map_err(|_| "remote conversation state poisoned")?;
        Ok(routes
            .get(route)
            .map_or((false, None), |entry| (entry.armed, entry.fallback.clone())))
    }

    pub(crate) fn retain_fallback(
        &self,
        route: RemoteRoute,
        binding: RemoteChatBinding,
    ) -> Result<(), String> {
        if route.channel != binding.channel || &*route.user != binding.user_id {
            return Err("remote fallback owner mismatch".into());
        }
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| "remote conversation state poisoned")?;
        routes.entry(route).or_default().fallback = Some(binding);
        Ok(())
    }

    pub(crate) fn clear(&self, route: &RemoteRoute) -> Result<(), String> {
        self.routes
            .lock()
            .map_err(|_| "remote conversation state poisoned")?
            .remove(route);
        Ok(())
    }

    pub(crate) fn clear_fallback(&self, route: &RemoteRoute) -> Result<(), String> {
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| "remote conversation state poisoned")?;
        if let Some(entry) = routes.get_mut(route) {
            entry.fallback = None;
            if !entry.armed {
                routes.remove(route);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_arming_is_scoped_by_channel_user_and_chat() {
        let state = RemoteConversations::default();
        let route = RemoteRoute {
            channel: RemoteChannel::Wechat,
            user: "user".into(),
            chat: "chat".into(),
        };
        state.arm(route.clone()).unwrap();
        for different in [
            RemoteRoute {
                channel: RemoteChannel::Test,
                ..route.clone()
            },
            RemoteRoute {
                user: "other".into(),
                ..route.clone()
            },
            RemoteRoute {
                chat: "other".into(),
                ..route.clone()
            },
        ] {
            assert!(!state.snapshot(&different).unwrap().0);
            state.clear(&different).unwrap();
        }
        assert!(state.snapshot(&route).unwrap().0);
        state.clear(&route).unwrap();
        assert!(!state.snapshot(&route).unwrap().0);
    }

    #[test]
    fn fallback_cannot_cross_principal_or_be_evicted_by_new_arming() {
        let state = RemoteConversations::default();
        let route = RemoteRoute {
            channel: RemoteChannel::Wechat,
            user: "owner".into(),
            chat: "chat".into(),
        };
        let binding = RemoteChatBinding {
            channel: RemoteChannel::Wechat,
            user_id: "owner".into(),
            session_id: crate::SessionId::new("session"),
        };
        let other = RemoteRoute {
            channel: RemoteChannel::Test,
            ..route.clone()
        };
        assert!(
            state
                .retain_fallback(other.clone(), binding.clone())
                .is_err()
        );
        state
            .retain_fallback(route.clone(), binding.clone())
            .unwrap();
        for n in 0..128 {
            state
                .arm(RemoteRoute {
                    chat: n.to_string().into(),
                    ..other.clone()
                })
                .unwrap();
        }
        assert_eq!(state.snapshot(&route).unwrap().1, Some(binding));
        assert_eq!(state.snapshot(&other).unwrap().1, None);
    }
}
