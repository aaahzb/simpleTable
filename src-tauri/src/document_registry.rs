//! 进程级文档会话、窗口所有权与租约授权。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::engine::{BigFile, BigMeta};

/// 文档命令需要的访问级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Cancel,
}

/// 文档迁移状态；迁移任务会逐步扩展 Sealing/Sealed/Accepted。
#[derive(Clone, Debug)]
pub enum DocState {
    Idle,
    Sealing,
    Sealed(u64),
    Accepted(u64),
    Closing,
}

/// 单个文档的可共享引擎与授权元数据。
#[derive(Clone)]
pub struct BigEntry {
    pub file: Arc<Mutex<BigFile>>,
    pub gate: Arc<AtomicU64>,
    pub load_cancel: Arc<AtomicBool>,
    pub owner: String,
    pub lease: u64,
    pub path: Option<PathBuf>,
    pub state: DocState,
}

impl BigEntry {
    fn new(file: BigFile, owner: &str, path: Option<PathBuf>) -> Self {
        Self {
            file: Arc::new(Mutex::new(file)),
            gate: Arc::new(AtomicU64::new(0)),
            load_cancel: Arc::new(AtomicBool::new(false)),
            owner: owner.to_string(),
            lease: 1,
            path,
            state: DocState::Idle,
        }
    }

    /// 让当前及更早的后台扫描失效。
    pub fn bump_gate(&self) -> u64 {
        self.gate.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// open/new 成功后由 Rust 分配的会话身份与元数据。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BigSession {
    pub session: u64,
    pub lease: u64,
    pub meta: BigMeta,
}

/// 进程内唯一的文档注册表。
#[derive(Default)]
pub struct DocumentRegistry {
    sessions: HashMap<u64, BigEntry>,
    next_session: u64,
}

impl DocumentRegistry {
    /// 登记新建引擎并分配进程唯一 session，初始 lease 为 1。
    pub fn insert_new(
        &mut self,
        file: BigFile,
        owner: &str,
        path: Option<PathBuf>,
    ) -> BigSession {
        let meta = file.meta();
        self.next_session = self.next_session.checked_add(1).expect("文档 session 已耗尽");
        let session = self.next_session;
        let lease = 1;
        self.sessions.insert(session, BigEntry::new(file, owner, path));
        BigSession { session, lease, meta }
    }

    /// 在元数据锁内校验 owner/lease 和访问级别，并克隆可等待的引擎句柄。
    pub fn authorize(
        &mut self,
        session: u64,
        owner: &str,
        lease: u64,
        access: Access,
    ) -> Result<BigEntry, String> {
        let entry = self
            .sessions
            .get(&session)
            .ok_or_else(|| format!("大文件会话未打开（session={session}）"))?;
        if entry.owner != owner {
            return Err(format!("窗口 {owner} 不是文档 session={session} 的 owner"));
        }
        if entry.lease != lease {
            return Err(format!("文档 lease 已过期（session={session}）"));
        }
        let allowed = match access {
            Access::Read => matches!(entry.state, DocState::Idle | DocState::Accepted(_)),
            Access::Write => matches!(entry.state, DocState::Idle),
            Access::Cancel => true,
        };
        if !allowed {
            return Err(format!("文档当前状态不允许 {access:?}（session={session}）"));
        }
        Ok(entry.clone())
    }

    /// 验证当前 owner/lease 后移除文档；调用方用先前克隆的句柄做锁外清理。
    pub fn close(&mut self, session: u64, owner: &str, lease: u64) -> Result<(), String> {
        self.authorize(session, owner, lease, Access::Read)?;
        if let Some(entry) = self.sessions.get_mut(&session) {
            entry.state = DocState::Closing;
        }
        self.sessions.remove(&session);
        Ok(())
    }

    /// 窗口是否已不拥有任何文档。
    pub fn owner_is_empty(&self, owner: &str) -> bool {
        !self.sessions.values().any(|entry| entry.owner == owner)
    }

    /// 关闭一个 owner 的全部文档；迁移中的文档阻止整窗关闭。
    pub fn close_owner(&mut self, owner: &str) -> Result<Vec<(u64, BigEntry)>, String> {
        if self.sessions.values().any(|entry| {
            entry.owner == owner && !matches!(entry.state, DocState::Idle)
        }) {
            return Err(format!("窗口 {owner} 存在正在迁移或关闭的文档"));
        }
        let sessions: Vec<u64> = self.sessions.iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(session, _)| *session)
            .collect();
        let mut closed = Vec::with_capacity(sessions.len());
        for session in sessions {
            if let Some(mut entry) = self.sessions.remove(&session) {
                entry.state = DocState::Closing;
                closed.push((session, entry));
            }
        }
        Ok(closed)
    }

    /// 进程退出时取走全部文档句柄，供锁外取消与等待。
    pub fn drain(&mut self) -> Vec<BigEntry> {
        self.sessions.drain().map(|(_, entry)| entry).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_process_unique_and_stale_owner_cannot_close_or_write() {
        let mut docs = DocumentRegistry::default();
        let a = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let b = docs.insert_new(BigFile::from_bytes(b"h\n2".to_vec(), false).unwrap(), "doc-1", None);
        assert_ne!(a.session, b.session);
        assert!(docs.authorize(a.session, "doc-1", a.lease, Access::Write).is_err());
        assert!(docs.authorize(a.session, "main", a.lease + 1, Access::Write).is_err());
        assert!(docs.close(a.session, "doc-1", a.lease).is_err());
        assert!(docs.close(a.session, "main", a.lease).is_ok());
        assert!(docs.authorize(b.session, "doc-1", b.lease, Access::Read).is_ok());
    }
}
