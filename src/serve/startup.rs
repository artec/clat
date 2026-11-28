use super::{ImBackend, ServeArgs};
use crate::TrustedProjectApplication;

pub(super) fn serve_wechat_credentials(
    trusted: &TrustedProjectApplication,
    args: &ServeArgs,
) -> Result<Option<crate::im::ilink::Credentials>, String> {
    if !matches!(args.im, Some(ImBackend::Wechat)) {
        return Ok(None);
    }
    trusted
        .wechat_binding()
        .map_err(|error| error.to_string())?
        .credentials
        .map(Some)
        .ok_or_else(|| {
            "WeChat IM is not configured yet; complete QR binding before starting with `--im wechat`"
                .to_owned()
        })
}

pub(super) fn serve_feishu_credentials(
    trusted: &TrustedProjectApplication,
    args: &ServeArgs,
) -> Result<Option<crate::im::feishu::Credentials>, crate::i18n::PresentedText> {
    if matches!(args.im, Some(ImBackend::Wechat)) {
        return Ok(None);
    }
    let snapshot = trusted
        .feishu_binding()
        .map_err(|error| crate::i18n::PresentedText::from(error.presentation()))?;
    if matches!(args.im, Some(ImBackend::Feishu)) {
        let credentials = snapshot.credentials.ok_or_else(|| {
            crate::i18n::PresentedText::from(crate::i18n::UiMessage::new("serve.feishu_unbound"))
        })?;
        trusted
            .set_feishu_receiving(true)
            .map_err(|error| crate::i18n::PresentedText::from(error.presentation()))?;
        return Ok(Some(credentials));
    }
    if !trusted
        .feishu_receiving()
        .map_err(|error| crate::i18n::PresentedText::from(error.presentation()))?
    {
        return Ok(None);
    }
    Ok(snapshot.credentials)
}
