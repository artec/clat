//! New image admission is optional; durable attachment resolution stays in Session.
use super::*;
mod runtime;
use crate::message::PendingMessage;
pub(crate) use runtime::DraftImages;
impl TrustedProjectApplication {
    pub(crate) fn image_runtime(&self) -> crate::application::DraftImages {
        self.draft_images.clone()
    }
    pub fn ensure_image_input_available(&self) -> Result<(), ApplicationError> {
        self.draft_images.require().map(|_| ())
    }
    pub(crate) fn ensure_user_images(
        &self,
        message: &PendingMessage,
    ) -> Result<(), ApplicationError> {
        if message.content.has_images()
            || !message.staged_attachments.is_empty()
            || !message.admitted_images.is_empty()
        {
            self.ensure_image_input_available()?;
        }
        Ok(())
    }
}
