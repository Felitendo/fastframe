#[cfg(any(target_os = "windows", test))]
use crate::{Notification, Sound};
#[cfg(any(target_os = "windows", test))]
use sha2::{Digest as _, Sha256};
#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_tags(key: &str) -> (String, String) {
    let hash: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (hash[..16].into(), hash[16..32].into())
}
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_xml(value: &Notification) -> String {
    let image = value
        .image
        .as_ref()
        .and_then(|path| url::Url::from_file_path(path).ok())
        .map(|url| {
            format!(
                "<image placement=\"appLogoOverride\" hint-crop=\"circle\" src=\"{}\"/>",
                escape(url.as_str())
            )
        })
        .unwrap_or_default();
    let actions: String = value
        .actions
        .iter()
        .filter(|(id, _)| id != "default")
        .map(|(id, label)| {
            format!(
                "<action content=\"{}\" arguments=\"{}\" activationType=\"foreground\"/>",
                escape(label),
                escape(id)
            )
        })
        .collect();
    let sound = if value.sound == Sound::Silent {
        "<audio silent=\"true\"/>"
    } else {
        "<audio src=\"ms-winsoundevent:Notification.Default\"/>"
    };
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text>{image}</binding></visual><actions>{actions}</actions>{sound}</toast>",
        escape(&value.title),
        escape(&value.body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_payload_cannot_inject_xml_or_actions() {
        let payload = windows_xml(
            &Notification::new("A < B")
                .body("a & b")
                .action("open\"x", "<open>")
                .sound(Sound::Silent),
        );
        assert!(payload.contains("A &lt; B"));
        assert!(payload.contains("a &amp; b"));
        assert!(payload.contains("arguments=\"open&quot;x\""));
        assert!(payload.contains("content=\"&lt;open&gt;\""));
        assert!(payload.contains("<audio silent=\"true\"/>"));
    }
    #[test]
    fn stable_windows_ids_fit_both_native_limits() {
        let (tag, group) = windows_tags("account-1/chat-42/reply-3");
        assert_eq!(tag.len(), 16);
        assert_eq!(group.len(), 16);
        assert_eq!(
            (tag.clone(), group.clone()),
            windows_tags("account-1/chat-42/reply-3")
        );
        assert_ne!((tag, group), windows_tags("account-2/chat-42/reply-3"));
    }
}

/// Bounded native ID history. The Linux session owns one instance, so a new
/// notification server starts with an empty history even if it reuses IDs.
#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
pub(crate) struct History(std::collections::VecDeque<(String, String, u32)>);
#[cfg(any(target_os = "linux", test))]
impl History {
    pub(crate) fn get(&self, app: &str, key: &str) -> Option<u32> {
        self.0
            .iter()
            .find(|(a, k, _)| a == app && k == key)
            .map(|(_, _, id)| *id)
    }
    pub(crate) fn remember(&mut self, app: &str, key: &str, id: u32) {
        self.0.retain(|(a, k, _)| a != app || k != key);
        self.0.push_back((app.into(), key.into(), id));
        while self.0.len() > 256 {
            self.0.pop_front();
        }
    }
    pub(crate) fn forget(&mut self, app: &str, key: &str, expected: u32) {
        self.0
            .retain(|(a, k, id)| a != app || k != key || *id != expected);
    }
}
#[cfg(test)]
mod history_tests {
    use super::*;
    #[test]
    fn retained_ids_are_bounded_and_scoped_to_app_and_generation() {
        let mut history = History::default();
        history.remember("one", "chat", 10);
        history.remember("two", "chat", 20);
        history.forget("one", "chat", 11);
        assert_eq!(history.get("one", "chat"), Some(10));
        history.forget("one", "chat", 10);
        assert_eq!(history.get("two", "chat"), Some(20));
        for id in 0..300 {
            history.remember("one", &id.to_string(), id);
        }
        assert_eq!(history.0.len(), 256);
        assert_eq!(history.get("two", "chat"), None);
        assert_eq!(history.get("one", "299"), Some(299));
    }
    #[test]
    fn plain_text_is_preserved_without_interpreting_markup() {
        assert_eq!(escape("<b>A & B</b>"), "&lt;b&gt;A &amp; B&lt;/b&gt;");
    }
}
