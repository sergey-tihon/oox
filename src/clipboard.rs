//! Editor clipboard shared by every buffer and, through the OS clipboard,
//! with other `oox` instances and applications.

use edtui::clipboard::{Clipboard, ClipboardTrait};

struct SystemClipboard {
    /// `None` when no OS clipboard is reachable (headless, SSH without a
    /// display, tests); yanks then stay within this process.
    system: Option<arboard::Clipboard>,
    /// Last text yanked in this process; pasted when the OS clipboard fails.
    fallback: String,
}

impl ClipboardTrait for SystemClipboard {
    fn set_text(&mut self, text: String) {
        if let Some(system) = self.system.as_mut() {
            let _ = system.set_text(text.as_str());
        }
        self.fallback = text;
    }

    fn get_text(&mut self) -> String {
        if let Some(system) = self.system.as_mut()
            && let Ok(text) = system.get_text()
        {
            return text;
        }
        self.fallback.clone()
    }
}

/// One clipboard handle for the whole app; clones share state.
pub fn shared() -> Clipboard {
    // Tests must not read or clobber the developer's real clipboard.
    let system = if cfg!(test) {
        None
    } else {
        arboard::Clipboard::new().ok()
    };
    Clipboard::new(SystemClipboard {
        system,
        fallback: String::new(),
    })
}
