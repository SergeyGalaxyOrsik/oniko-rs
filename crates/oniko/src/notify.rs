//! Desktop notifications about task agents. Clicking one opens its thread.

use std::sync::Arc;

use oniko_core::core::{CoreHandle, Notify, TaskNotice};

/// The core is created before the window, so the handle arrives once it exists.
pub fn notifier(handle: Arc<std::sync::OnceLock<CoreHandle>>) -> Notify {
    Arc::new(move |notice: TaskNotice| {
        let handle = handle.clone();
        // Showing waits for the click, so each notification gets its own thread.
        let _ = std::thread::Builder::new().name("oniko-notification".into()).spawn(move || {
            if show(&notice)
                && let Some(core) = handle.get()
            {
                let id = notice.thread_id.clone();
                core.send(move |core| core.open_thread(id));
            }
        });
    })
}

/// True when the user clicked it.
#[cfg(target_os = "macos")]
fn show(notice: &TaskNotice) -> bool {
    use mac_notification_sys::{Notification, NotificationResponse, get_bundle_identifier_or_default, set_application};
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // Run from a bundle, notifications carry Oniko's icon; unbundled, they borrow Finder's.
        let _ = set_application(&get_bundle_identifier_or_default("Oniko"));
    });
    matches!(
        Notification::new().title(&notice.title).message(&notice.body).wait_for_click(true).send(),
        Ok(NotificationResponse::Click | NotificationResponse::ActionButton(_))
    )
}

#[cfg(not(target_os = "macos"))]
fn show(notice: &TaskNotice) -> bool {
    let mut clicked = false;
    if let Ok(handle) = notify_rust::Notification::new().summary(&notice.title).body(&notice.body).action("default", "Open").show() {
        #[cfg(all(unix, not(target_os = "macos")))]
        handle.wait_for_action(|action| clicked = action == "default");
        #[cfg(windows)]
        let _ = handle;
    }
    clicked
}
