//! Device-wide window presentation. No Host or conversation state.
use clat_core::client_ports::presentation::{UiMessage, load_ui_preferences, save_ui_preferences};
use serde_json::{Value, json};
use tauri::Manager;

fn strength(preferences: &Value) -> u8 {
    preferences["backgroundBlurStrength"]
        .as_u64()
        .unwrap_or_else(|| {
            if preferences["backgroundBlur"].as_bool().unwrap_or(true) {
                25
            } else {
                0
            }
        })
        .min(100) as u8
}

fn transparency_value(preferences: &Value, requested: Option<u8>) -> u64 {
    requested
        .map(u64::from)
        .unwrap_or_else(|| preferences["backgroundTransparency"].as_u64().unwrap_or(0))
        .min(100)
}

#[tauri::command]
pub async fn desktop_background(
    window: tauri::WebviewWindow,
    transparency: Option<u8>,
    blur: Option<bool>,
    blur_strength: Option<u8>,
    preview: Option<bool>,
) -> Result<Value, UiMessage> {
    crate::commands::guard(&window)?;
    let path = crate::localization::path(window.app_handle())?;
    let preferences = load_ui_preferences(&path)?;
    let value = transparency_value(&preferences, transparency);
    let blur_value = blur_strength
        .map(|v| v.min(100))
        .or_else(|| blur.map(|v| if v { 25 } else { 0 }))
        .unwrap_or_else(|| strength(&preferences));
    apply(&window, if value > 0 { blur_value } else { 0 }).await?;
    if !preview.unwrap_or(false)
        && (transparency.is_some() || blur.is_some() || blur_strength.is_some())
        && let Err(error) = save_ui_preferences(
            &path,
            &json!({
                "backgroundTransparency":value,"backgroundBlur":blur_value > 0,
                "backgroundBlurStrength":blur_value
            }),
        )
    {
        let previous = if preferences["backgroundTransparency"].as_u64().unwrap_or(0) > 0 {
            strength(&preferences)
        } else {
            0
        };
        let _ = apply(&window, previous).await;
        return Err(error.into());
    }
    Ok(
        json!({"transparency":value,"blur":blur_value > 0,"blurStrength":blur_value,
        "blurAdjustable":adjustable()}),
    )
}

fn adjustable() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::available()
    }
    #[cfg(windows)]
    {
        false
    }
}

async fn apply(window: &tauri::WebviewWindow, strength: u8) -> Result<(), UiMessage> {
    #[cfg(target_os = "macos")]
    if adjustable() {
        window.set_effects(None).map_err(|e| e.to_string())?;
        return macos::apply(window, strength).await;
    }
    use tauri::window::{Effect, EffectState, EffectsBuilder};
    if strength == 0 {
        return window.set_effects(None).map_err(|e| e.to_string().into());
    }
    #[cfg(target_os = "macos")]
    let effect = Effect::UnderWindowBackground;
    #[cfg(windows)]
    let effect = Effect::Acrylic;
    window
        .set_effects(
            EffectsBuilder::new()
                .effect(effect)
                .state(EffectState::Active)
                .build(),
        )
        .map_err(|e| e.to_string().into())
}

#[cfg(target_os = "macos")]
#[path = "background/macos.rs"]
mod macos;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_background_transparency_is_supported_without_changing_old_preferences() {
        assert_eq!(transparency_value(&json!({}), Some(100)), 100);
        assert_eq!(
            transparency_value(&json!({"backgroundTransparency":80}), None),
            80
        );
        assert_eq!(
            transparency_value(&json!({"backgroundTransparency":100}), None),
            100
        );
        assert_eq!(transparency_value(&json!({}), None), 0);
        assert_eq!(transparency_value(&json!({}), Some(255)), 100);
    }
    #[test]
    fn blur_preference_preserves_legacy_switch_and_bounds_strength() {
        assert_eq!(strength(&json!({})), 25);
        assert_eq!(strength(&json!({"backgroundBlur":false})), 0);
        assert_eq!(strength(&json!({"backgroundBlur":true})), 25);
        assert_eq!(
            strength(&json!({"backgroundBlur":true,"backgroundBlurStrength":0})),
            0
        );
        assert_eq!(strength(&json!({"backgroundBlurStrength":57})), 57);
        assert_eq!(strength(&json!({"backgroundBlurStrength":200})), 100);
    }
}
