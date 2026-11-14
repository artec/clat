use super::ControlStorage;

const LIMIT: usize = 128 * 1024 * 1024;

impl ControlStorage {
    pub(crate) fn read_acp(&self) -> Result<Option<String>, String> {
        crate::private_fs::read_private_text(&self.dir, "acp.json", LIMIT)
    }

    pub(crate) fn write_acp(&self, text: &str) -> Result<(), String> {
        if text.len() > LIMIT {
            return Err("ACP storage limit reached".into());
        }
        crate::private_fs::write_text_atomic_in_dir(&self.dir, "acp.json", text)
    }
}
