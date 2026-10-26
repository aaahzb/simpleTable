//! 进程内窗口角色、就绪状态、关闭状态与最近活动路由。

use std::collections::HashMap;

use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRole {
    Primary,
    Document,
    Diagnostic,
}

struct WindowEntry {
    role: WindowRole,
    ready: bool,
    closing: bool,
    activity: u64,
}

#[derive(Default)]
pub struct WindowRegistry {
    entries: HashMap<String, WindowEntry>,
    ready_waiters: HashMap<String, oneshot::Sender<()>>,
    next_document: u64,
    activity: u64,
}

impl WindowRegistry {
    fn next_activity(&mut self) -> u64 {
        self.activity = self.activity.checked_add(1).expect("窗口活动序号已耗尽");
        self.activity
    }

    pub fn register(&mut self, label: &str, role: WindowRole) {
        let activity = self.next_activity();
        self.entries.insert(label.to_string(), WindowEntry {
            role,
            ready: false,
            closing: false,
            activity,
        });
    }

    pub fn allocate_document_label(&mut self) -> String {
        loop {
            self.next_document = self.next_document.checked_add(1).expect("窗口 label 已耗尽");
            let label = format!("doc-{}", self.next_document);
            if !self.entries.contains_key(&label) {
                return label;
            }
        }
    }

    pub fn wait_ready(&mut self, label: &str, waiter: oneshot::Sender<()>) {
        if self.entries.get(label).is_some_and(|entry| entry.ready) {
            let _ = waiter.send(());
        } else {
            self.ready_waiters.insert(label.to_string(), waiter);
        }
    }

    pub fn mark_ready(&mut self, label: &str) -> bool {
        if !self.entries.contains_key(label) {
            return false;
        }
        if let Some(entry) = self.entries.get_mut(label) {
            entry.ready = true;
        }
        self.touch(label);
        if let Some(waiter) = self.ready_waiters.remove(label) {
            let _ = waiter.send(());
        }
        true
    }

    pub fn touch(&mut self, label: &str) {
        let activity = self.next_activity();
        if let Some(entry) = self.entries.get_mut(label) {
            entry.activity = activity;
        }
    }

    pub fn recent_ready(&self) -> Option<String> {
        self.entries.iter()
            .filter(|(_, entry)| entry.ready && !entry.closing)
            .max_by_key(|(_, entry)| entry.activity)
            .map(|(label, _)| label.clone())
    }

    pub fn recent_registered(&self) -> Option<String> {
        self.entries.iter()
            .filter(|(_, entry)| !entry.closing)
            .max_by_key(|(_, entry)| entry.activity)
            .map(|(label, _)| label.clone())
    }

    pub fn is_ready(&self, label: &str) -> bool {
        self.entries.get(label).is_some_and(|entry| entry.ready && !entry.closing)
    }

    pub fn role(&self, label: &str) -> Option<WindowRole> {
        self.entries.get(label).map(|entry| entry.role)
    }

    pub fn begin_close(&mut self, label: &str) -> Result<(), String> {
        let entry = self.entries.get_mut(label)
            .ok_or_else(|| format!("窗口未注册：{label}"))?;
        if entry.closing {
            return Err(format!("窗口正在关闭：{label}"));
        }
        entry.closing = true;
        Ok(())
    }

    pub fn cancel_close(&mut self, label: &str) {
        if let Some(entry) = self.entries.get_mut(label) {
            entry.closing = false;
        }
    }

    pub fn unregister(&mut self, label: &str) -> bool {
        self.ready_waiters.remove(label);
        self.entries.remove(label).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{WindowRegistry, WindowRole};
    use crate::document_registry::{Access, DocumentRegistry};
    use crate::engine::BigFile;

    #[test]
    fn ready_lookup_excludes_unready_and_closing_windows() {
        let mut windows = WindowRegistry::default();
        windows.register("main", WindowRole::Primary);
        windows.register("doc-1", WindowRole::Document);
        assert!(!windows.is_ready("doc-1"));
        windows.mark_ready("doc-1");
        assert!(windows.is_ready("doc-1"));
        windows.begin_close("doc-1").unwrap();
        assert!(!windows.is_ready("doc-1"));
    }

    #[test]
    fn recent_ready_skips_unready_and_unregistered_windows() {
        let mut windows = WindowRegistry::default();
        windows.register("main", WindowRole::Primary);
        windows.register("doc-1", WindowRole::Document);
        windows.mark_ready("main");
        windows.touch("doc-1");
        assert_eq!(windows.recent_ready().as_deref(), Some("main"));
        windows.mark_ready("doc-1");
        windows.touch("doc-1");
        assert_eq!(windows.recent_ready().as_deref(), Some("doc-1"));
        windows.unregister("doc-1");
        assert_eq!(windows.recent_ready().as_deref(), Some("main"));
    }

    #[test]
    fn closing_one_window_removes_only_its_documents() {
        let mut docs = DocumentRegistry::default();
        let a = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let b = docs.insert_new(BigFile::from_bytes(b"h\n2".to_vec(), false).unwrap(), "doc-1", None);
        docs.close_owner("doc-1").unwrap();
        assert!(docs.authorize(a.session, "main", a.lease, Access::Read).is_ok());
        assert!(docs.authorize(b.session, "doc-1", b.lease, Access::Read).is_err());
    }
}
