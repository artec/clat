//! Model activation is one settings commit, including its profile pointer.
//! Multiple project scopes must never observe another scope's half-activation.
use super::*;

impl ControlStorage {
    pub(crate) fn activate_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
        let name = name.trim();
        let state = self.lock();
        let Some(profile) = state.settings.profiles.get(name) else {
            return Ok(None);
        };
        let pair = decode_config_pair(&profile.config, &profile.runtime)?;
        let row = settings::ModelStateRow {
            config: profile.config.clone(),
            runtime: profile.runtime.clone(),
            active_profile: Some(name.to_owned()),
            updated_at: timestamp::now_iso8601(),
        };
        self.commit(
            state,
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| state.settings.model_state = Some(row.clone()),
        )?;
        Ok(pair)
    }

    pub(crate) fn delete_profile_with_fallback(&self, name: &str) -> Result<(), ControlError> {
        let name = name.trim();
        if is_vendor_slot(name) {
            return Err(control_error("profile name prefix `vendor:` is reserved"));
        }
        let state = self.lock();
        let active = state
            .settings
            .model_state
            .as_ref()
            .and_then(|row| row.active_profile.as_deref())
            == Some(name);
        let replacement = if active {
            Some(fallback_row(&state, name)?)
        } else {
            None
        };
        self.commit(
            state,
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| {
                state.settings.profiles.remove(name);
                clear_utility_profile(&mut state.settings, name);
                if let Some(row) = &replacement {
                    state.settings.model_state = Some(row.clone());
                }
            },
        )
    }

    pub(crate) fn utility_settings(&self) -> Result<settings::UtilitySettingsRow, ControlError> {
        let state = self.lock();
        let utility = state.settings.utility.clone();
        if let Some(profile) = utility.profile.as_deref()
            && !state.settings.profiles.contains_key(profile)
        {
            return Err(control_error("utility profile does not exist"));
        }
        Ok(utility)
    }

    pub(crate) fn set_utility_settings(
        &self,
        utility: settings::UtilitySettingsRow,
    ) -> Result<(), ControlError> {
        let state = self.lock();
        if let Some(profile) = utility.profile.as_deref()
            && (!valid_profile_name(profile) || !state.settings.profiles.contains_key(profile))
        {
            return Err(control_error("utility profile does not exist"));
        }
        self.commit(
            state,
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| state.settings.utility = utility,
        )
    }
}

pub(super) fn clear_utility_profile(settings: &mut settings::SettingsFile, removed: &str) {
    if settings.utility.profile.as_deref() == Some(removed) {
        settings.utility.profile = None;
    }
}

fn valid_profile_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && name.len() <= 128
        && !is_vendor_slot(name)
        && !name.chars().any(char::is_control)
}

fn fallback_row(
    state: &ControlState,
    removed: &str,
) -> Result<settings::ModelStateRow, ControlError> {
    let remaining = state
        .settings
        .profiles
        .iter()
        .find(|(name, _)| name.as_str() != removed && !is_vendor_slot(name));
    let (config, runtime, active_profile) = if let Some((name, profile)) = remaining {
        decode_config_pair(&profile.config, &profile.runtime)?;
        (
            profile.config.clone(),
            profile.runtime.clone(),
            Some(name.clone()),
        )
    } else {
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let (config, runtime) = encode_config_pair(&config, &credentials)?;
        (config, runtime, None)
    };
    Ok(settings::ModelStateRow {
        config,
        runtime,
        active_profile,
        updated_at: timestamp::now_iso8601(),
    })
}
