use super::*;
impl ControlStorage {
    pub(crate) fn read_schedules(&self) -> Result<Option<String>, String> {
        let _guard = self.lock();
        crate::private_fs::read_private_text(&self.dir, "schedules.json", 2 * 1024 * 1024)
    }
    pub(crate) fn save_schedules(&self, text: &str) -> Result<(), String> {
        let _guard = self.lock();
        crate::private_fs::write_text_atomic_in_dir(&self.dir, "schedules.json", text)
    }
}
