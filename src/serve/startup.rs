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
