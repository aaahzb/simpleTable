//! 进程级文档会话、窗口所有权与租约授权。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::engine::{BigFile, BigMeta};

/// 文档命令需要的访问级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Cancel,
}

/// 文档迁移状态。
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

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
struct Transfer {
    session: u64,
    meta: BigMeta,
    source: String,
    target: String,
    side: String,
    index: usize,
    snapshot: serde_json::Value,
    deadline: Instant,
    adopted: bool,
    source_gone: bool,
}

/// 目标接管同一 BigFile 所需的全部会话信息。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedSession {
    pub ticket: u64,
    pub session: u64,
    pub lease: u64,
    pub meta: BigMeta,
    pub path: Option<PathBuf>,
    pub side: String,
    pub index: usize,
    pub snapshot: serde_json::Value,
}

/// 迁移两端及 ticket 的路由快照。
#[derive(Clone)]
pub struct TransferRoute {
    pub ticket: u64,
    pub session: u64,
    pub source: String,
    pub target: String,
}

/// complete/cancel 后用于释放打开队列和定向通知的最终 owner/lease。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferUpdate {
    pub ticket: u64,
    pub session: u64,
    pub owner: String,
    pub lease: u64,
}

/// 目标完成 adopt 后，源是否仍可完成最后一步。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedNotice {
    pub ticket: u64,
    pub session: u64,
    pub lease: u64,
    pub source_live: bool,
}

/// 窗口意外销毁后对每个迁移文档采取的恢复动作。
pub enum DestroyOutcome {
    Cancelled {
        ticket: u64,
        session: u64,
        owner: String,
        lease: u64,
    },
    Detached {
        ticket: u64,
        session: u64,
        owner: String,
        lease: u64,
    },
    Completed {
        ticket: u64,
        session: u64,
        owner: String,
        lease: u64,
    },
    Recovered {
        ticket: u64,
        session: u64,
        owner: String,
        lease: u64,
        adopted: AdoptedSession,
    },
    Dropped {
        ticket: Option<u64>,
        session: u64,
        owner: String,
        lease: u64,
        entry: Option<BigEntry>,
    },
}

/// 进程内唯一的文档注册表。
#[derive(Default)]
pub struct DocumentRegistry {
    sessions: HashMap<u64, BigEntry>,
    transfers: HashMap<u64, Transfer>,
    next_session: u64,
    next_ticket: u64,
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

    /// 封存源文档；文件锁屏障成功后才发布 ticket。
    pub fn seal(
        &mut self,
        session: u64,
        source: &str,
        lease: u64,
        target: &str,
        side: &str,
        index: usize,
        snapshot: serde_json::Value,
    ) -> Result<u64, String> {
        if source == target {
            return Err("源窗口和目标窗口不能相同".into());
        }
        let file = {
            let entry = self.sessions.get_mut(&session)
                .ok_or_else(|| format!("大文件会话未打开（session={session}）"))?;
            if entry.owner != source || entry.lease != lease {
                return Err("文档 owner 或 lease 已过期".into());
            }
            if !matches!(entry.state, DocState::Idle) {
                return Err("文档已在迁移或关闭中".into());
            }
            entry.state = DocState::Sealing;
            entry.bump_gate();
            entry.file.clone()
        };
        let guard = match file.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                self.restore_failed_seal(session, source, lease);
                return Err("当前操作尚未完成，请稍后重试".into());
            }
            Err(TryLockError::Poisoned(error)) => {
                self.restore_failed_seal(session, source, lease);
                return Err(format!("文档锁已损坏：{error}"));
            }
        };
        {
            let entry = self.sessions.get(&session)
                .ok_or_else(|| "封存期间文档被移除".to_string())?;
            if entry.owner != source
                || entry.lease != lease
                || !matches!(entry.state, DocState::Sealing)
            {
                return Err("封存期间文档 owner、lease 或状态已变化".into());
            }
        }
        let meta = guard.meta();
        self.next_ticket = self.next_ticket.checked_add(1).expect("迁移 ticket 已耗尽");
        let ticket = self.next_ticket;
        self.transfers.insert(ticket, Transfer {
            session,
            meta,
            source: source.to_string(),
            target: target.to_string(),
            side: side.to_string(),
            index,
            snapshot,
            deadline: Instant::now() + TRANSFER_TIMEOUT,
            adopted: false,
            source_gone: false,
        });
        self.sessions.get_mut(&session).expect("封存后文档必须存在").state =
            DocState::Sealed(ticket);
        drop(guard);
        Ok(ticket)
    }

    fn restore_failed_seal(&mut self, session: u64, source: &str, lease: u64) {
        if let Some(entry) = self.sessions.get_mut(&session) {
            if entry.owner == source
                && entry.lease == lease
                && matches!(entry.state, DocState::Sealing)
            {
                entry.state = DocState::Idle;
            }
        }
    }

    fn transfer_route(&self, ticket: u64) -> Option<TransferRoute> {
        self.transfers.get(&ticket).map(|transfer| TransferRoute {
            ticket,
            session: transfer.session,
            source: transfer.source.clone(),
            target: transfer.target.clone(),
        })
    }

    /// ticket 对应的 source/target/session 路由快照。
    pub fn route(&self, ticket: u64) -> Option<TransferRoute> {
        self.transfer_route(ticket)
    }

    /// owner 是否还有未完成的转出，正常关闭源窗口必须拒绝。
    pub fn has_outgoing(&self, owner: &str) -> bool {
        self.transfers.values().any(|transfer| transfer.source == owner)
    }

    /// 目标接受同一 BigFile；Accepted 阶段只读，直到 complete。
    pub fn accept(&mut self, ticket: u64, target: &str) -> Result<AdoptedSession, String> {
        let transfer = self.transfers.get(&ticket).cloned()
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        if transfer.target != target {
            return Err("窗口不是该迁移的目标".into());
        }
        let (path, old_lease) = {
            let entry = self.sessions.get(&transfer.session)
                .ok_or_else(|| "迁移文档已移除".to_string())?;
            if entry.owner != transfer.source || !matches!(entry.state, DocState::Sealed(t) if t == ticket) {
                return Err("文档不在 Sealed 状态或 owner 已变化".into());
            }
            (entry.path.clone(), entry.lease)
        };
        let lease = old_lease.checked_add(1).ok_or_else(|| "文档 lease 已耗尽".to_string())?;
        let meta = transfer.meta.clone();
        let entry = self.sessions.get_mut(&transfer.session)
            .ok_or_else(|| "迁移文档已移除".to_string())?;
        if entry.owner != transfer.source
            || entry.lease != old_lease
            || !matches!(entry.state, DocState::Sealed(t) if t == ticket)
        {
            return Err("接受期间文档 owner、lease 或状态已变化".into());
        }
        entry.owner = target.to_string();
        entry.lease = lease;
        entry.state = DocState::Accepted(ticket);
        Ok(AdoptedSession {
            ticket,
            session: transfer.session,
            lease,
            meta,
            path,
            side: transfer.side,
            index: transfer.index,
            snapshot: transfer.snapshot,
        })
    }

    /// 目标完成模型和标签接管；源已销毁时立即完成，否则等待 source complete。
    pub fn mark_adopted(&mut self, ticket: u64, target: &str) -> Result<AdoptedNotice, String> {
        let transfer = self.transfers.get(&ticket).cloned()
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        if transfer.target != target {
            return Err("窗口不是该迁移的目标".into());
        }
        let entry = self.sessions.get_mut(&transfer.session)
            .ok_or_else(|| "迁移文档已移除".to_string())?;
        if entry.owner != target || !matches!(entry.state, DocState::Accepted(t) if t == ticket) {
            return Err("文档不在该目标的 Accepted 状态".into());
        }
        let lease = entry.lease;
        let source_live = !transfer.source_gone;
        if source_live {
            self.transfers.get_mut(&ticket).expect("ticket 已校验").adopted = true;
        } else {
            entry.state = DocState::Idle;
            self.transfers.remove(&ticket);
        }
        Ok(AdoptedNotice {
            ticket,
            session: transfer.session,
            lease,
            source_live,
        })
    }

    /// 源确认目标已 adopt，保持 accepted lease 并恢复目标可写。
    pub fn complete(&mut self, ticket: u64, source: &str) -> Result<(u64, String, u64), String> {
        let transfer = self.transfers.get(&ticket).cloned()
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        if transfer.source != source {
            return Err("只有源窗口可以完成迁移".into());
        }
        if transfer.source_gone || !transfer.adopted {
            return Err("目标尚未完成 adopt，不能完成迁移".into());
        }
        let entry = self.sessions.get_mut(&transfer.session)
            .ok_or_else(|| "迁移文档已移除".to_string())?;
        if entry.owner != transfer.target
            || !matches!(entry.state, DocState::Accepted(t) if t == ticket)
        {
            return Err("文档不在目标的 Accepted 状态".into());
        }
        entry.state = DocState::Idle;
        let owner = entry.owner.clone();
        let lease = entry.lease;
        self.transfers.remove(&ticket);
        Ok((transfer.session, owner, lease))
    }

    /// 回滚到 source，并再次提升 lease 使目标旧授权和旧事件失效。
    pub fn cancel(&mut self, ticket: u64, actor: &str) -> Result<(u64, String, u64), String> {
        let transfer = self.transfers.get(&ticket).cloned()
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        if actor != transfer.source && actor != transfer.target {
            return Err("只有迁移源或目标可以取消".into());
        }
        if transfer.source_gone {
            return Err("源窗口已销毁，不能回滚到该窗口".into());
        }
        let entry = self.sessions.get_mut(&transfer.session)
            .ok_or_else(|| "迁移文档已移除".to_string())?;
        if !matches!(entry.state, DocState::Sealed(t) | DocState::Accepted(t) if t == ticket) {
            return Err("文档不在可取消的迁移状态".into());
        }
        entry.lease = entry.lease.checked_add(1).ok_or_else(|| "文档 lease 已耗尽".to_string())?;
        entry.owner = transfer.source.clone();
        entry.state = DocState::Idle;
        entry.bump_gate();
        let lease = entry.lease;
        self.transfers.remove(&ticket);
        Ok((transfer.session, transfer.source, lease))
    }

    /// 正常关闭目标前先取消所有转入；源有未决转出由 has_outgoing 单独拒绝。
    pub fn cancel_incoming(&mut self, target: &str) -> Result<Vec<TransferUpdate>, String> {
        let mut tickets: Vec<u64> = self.transfers.iter()
            .filter(|(_, transfer)| transfer.target == target)
            .map(|(ticket, _)| *ticket)
            .collect();
        tickets.sort_unstable();
        for ticket in &tickets {
            let transfer = self.transfers.get(ticket).expect("ticket 来自当前表");
            if transfer.source_gone {
                return Err("源窗口已销毁，不能回滚转入文档".into());
            }
        }
        let mut updates = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            let (session, owner, lease) = self.cancel(ticket, target)?;
            updates.push(TransferUpdate { ticket, session, owner, lease });
        }
        Ok(updates)
    }

    /// 返回已到期且仍可回滚到存活源的 ticket 路由，顺序与 reap_expired 一致。
    pub fn expired_routes(&self, now: Instant) -> Vec<TransferRoute> {
        let mut routes: Vec<TransferRoute> = self.transfers.iter()
            .filter(|(_, transfer)| !transfer.source_gone && transfer.deadline <= now)
            .map(|(ticket, transfer)| TransferRoute {
                ticket: *ticket,
                session: transfer.session,
                source: transfer.source.clone(),
                target: transfer.target.clone(),
            })
            .collect();
        routes.sort_by_key(|route| route.ticket);
        routes
    }

    /// 回收可回滚的到期迁移；源已销毁的 Detached 只能由 adopt 或目标销毁收尾。
    pub fn reap_expired(&mut self, now: Instant) -> Vec<(u64, String, u64)> {
        let routes = self.expired_routes(now);
        let mut expired = Vec::with_capacity(routes.len());
        for route in routes {
            if let Ok((session, owner, lease)) = self.cancel(route.ticket, &route.source) {
                expired.push((session, owner, lease));
            }
        }
        expired
    }

    fn recover(&mut self, ticket: u64, owner: &str) -> Result<DestroyOutcome, String> {
        let transfer = self.transfers.get(&ticket).cloned()
            .ok_or_else(|| "迁移 ticket 不存在或已结束".to_string())?;
        let (path, old_lease) = {
            let entry = self.sessions.get(&transfer.session)
                .ok_or_else(|| "迁移文档已移除".to_string())?;
            if !matches!(entry.state, DocState::Sealed(t) | DocState::Accepted(t) if t == ticket) {
                return Err("文档不在可恢复的迁移状态".into());
            }
            (entry.path.clone(), entry.lease)
        };
        let lease = old_lease.checked_add(1).ok_or_else(|| "文档 lease 已耗尽".to_string())?;
        let meta = transfer.meta.clone();
        let entry = self.sessions.get_mut(&transfer.session)
            .ok_or_else(|| "迁移文档已移除".to_string())?;
        entry.owner = owner.to_string();
        entry.lease = lease;
        entry.state = DocState::Idle;
        entry.bump_gate();
        let adopted = AdoptedSession {
            ticket,
            session: transfer.session,
            lease,
            meta,
            path,
            side: transfer.side,
            index: transfer.index,
            snapshot: transfer.snapshot,
        };
        self.transfers.remove(&ticket);
        Ok(DestroyOutcome::Recovered {
            ticket,
            session: transfer.session,
            owner: owner.to_string(),
            lease,
            adopted,
        })
    }

    fn dropped(&self, ticket: u64) -> Option<DestroyOutcome> {
        let transfer = self.transfers.get(&ticket)?;
        let entry = self.sessions.get(&transfer.session)?;
        Some(DestroyOutcome::Dropped {
            ticket: Some(ticket),
            session: transfer.session,
            owner: entry.owner.clone(),
            lease: entry.lease,
            entry: None,
        })
    }

    /// 窗口异常销毁：按存活源/目标/最近 ready fallback 恢复或回滚迁移。
    pub fn destroy_window(
        &mut self,
        label: &str,
        live: &HashSet<String>,
        fallback: Option<&str>,
    ) -> Vec<DestroyOutcome> {
        let fallback = fallback.filter(|owner| live.contains(*owner) && *owner != label);
        let mut tickets: Vec<u64> = self.transfers.iter()
            .filter(|(_, transfer)| transfer.source == label || transfer.target == label)
            .map(|(ticket, _)| *ticket)
            .collect();
        tickets.sort_unstable();
        let mut outcomes = Vec::new();
        for ticket in tickets {
            let Some(transfer) = self.transfers.get(&ticket).cloned() else {
                continue;
            };
            let outcome = if transfer.target == label {
                if live.contains(&transfer.source) {
                    match self.cancel(ticket, label) {
                        Ok((session, owner, lease)) => DestroyOutcome::Cancelled {
                            ticket,
                            session,
                            owner,
                            lease,
                        },
                        Err(_) => match self.dropped(ticket) {
                            Some(outcome) => outcome,
                            None => continue,
                        },
                    }
                } else if let Some(owner) = fallback {
                    match self.recover(ticket, owner) {
                        Ok(outcome) => outcome,
                        Err(_) => match self.dropped(ticket) {
                            Some(outcome) => outcome,
                            None => continue,
                        },
                    }
                } else {
                    match self.dropped(ticket) {
                        Some(outcome) => outcome,
                        None => continue,
                    }
                }
            } else if transfer.source == label {
                if live.contains(&transfer.target) {
                    let adopted = transfer.adopted;
                    let state = self.sessions.get(&transfer.session)
                        .map(|entry| entry.state.clone());
                    match state {
                        Some(DocState::Sealed(t)) if t == ticket => {
                            match self.recover(ticket, &transfer.target) {
                                Ok(outcome) => outcome,
                                Err(_) => match self.dropped(ticket) {
                                    Some(outcome) => outcome,
                                    None => continue,
                                },
                            }
                        }
                        Some(DocState::Accepted(t)) if t == ticket && adopted => {
                            match self.complete(ticket, label) {
                                Ok((session, owner, lease)) => DestroyOutcome::Completed {
                                    ticket,
                                    session,
                                    owner,
                                    lease,
                                },
                                Err(_) => match self.dropped(ticket) {
                                    Some(outcome) => outcome,
                                    None => continue,
                                },
                            }
                        }
                        Some(DocState::Accepted(t)) if t == ticket => {
                            self.transfers.get_mut(&ticket).expect("ticket 已校验").source_gone = true;
                            let entry = self.sessions.get(&transfer.session)
                                .expect("Accepted 文档必须存在");
                            DestroyOutcome::Detached {
                                ticket,
                                session: transfer.session,
                                owner: entry.owner.clone(),
                                lease: entry.lease,
                            }
                        }
                        _ => match self.dropped(ticket) {
                            Some(outcome) => outcome,
                            None => continue,
                        },
                    }
                } else if let Some(owner) = fallback {
                    match self.recover(ticket, owner) {
                        Ok(outcome) => outcome,
                        Err(_) => match self.dropped(ticket) {
                            Some(outcome) => outcome,
                            None => continue,
                        },
                    }
                } else {
                    match self.dropped(ticket) {
                        Some(outcome) => outcome,
                        None => continue,
                    }
                }
            } else {
                continue;
            };
            outcomes.push(outcome);
        }
        let idle_sessions: Vec<u64> = self.sessions.iter()
            .filter(|(_, entry)| entry.owner == label && matches!(entry.state, DocState::Idle))
            .map(|(session, _)| *session)
            .collect();
        for session in idle_sessions {
            if let Some(mut entry) = self.sessions.remove(&session) {
                entry.state = DocState::Closing;
                outcomes.push(DestroyOutcome::Dropped {
                    ticket: None,
                    session,
                    owner: entry.owner.clone(),
                    lease: entry.lease,
                    entry: Some(entry),
                });
            }
        }
        outcomes
    }

    /// 进程退出时取走全部文档句柄，供锁外取消与等待。
    pub fn drain(&mut self) -> Vec<BigEntry> {
        self.transfers.clear();
        self.sessions.drain().map(|(_, entry)| entry).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Approval;
    use std::time::{Duration, Instant};

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

    #[test]
    fn accept_is_read_only_complete_preserves_same_bigfile_and_history() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\nold\n".to_vec(), false).unwrap(), "main", None);
        let original = docs.authorize(opened.session, "main", opened.lease, Access::Write).unwrap();
        original.file.lock().unwrap().set_cell(0, 0, "new".into(), Approval::deny()).unwrap();
        let original_arc = original.file.clone();
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({"filter":{"cols":[]}})).unwrap();
        let adopted = docs.accept(ticket, "doc-1").unwrap();
        assert_ne!(adopted.lease, opened.lease);
        assert!(docs.authorize(opened.session, "main", opened.lease, Access::Write).is_err());
        assert!(docs.authorize(opened.session, "doc-1", adopted.lease, Access::Read).is_ok());
        assert!(docs.authorize(opened.session, "doc-1", adopted.lease, Access::Write).is_err());
        let notice = docs.mark_adopted(ticket, "doc-1").unwrap();
        assert!(notice.source_live);
        docs.complete(ticket, "main").unwrap();
        let current = docs.authorize(opened.session, "doc-1", adopted.lease, Access::Write).unwrap();
        assert!(Arc::ptr_eq(&original_arc, &current.file));
        assert!(current.file.lock().unwrap().undo());
    }

    #[test]
    fn cancel_after_accept_restores_source_and_invalidates_target_lease() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let accepted = docs.accept(ticket, "doc-1").unwrap();
        let (_, owner, rollback_lease) = docs.cancel(ticket, "doc-1").unwrap();
        assert_eq!(owner, "main");
        assert!(rollback_lease > accepted.lease);
        assert!(docs.authorize(opened.session, "doc-1", accepted.lease, Access::Write).is_err());
        assert!(docs.authorize(opened.session, "main", rollback_lease, Access::Write).is_ok());
    }

    #[test]
    fn timeout_rolls_back_after_accept() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let accepted = docs.accept(ticket, "doc-1").unwrap();
        let expired = docs.reap_expired(Instant::now() + Duration::from_secs(16));
        assert_eq!(expired.len(), 1);
        assert!(docs.authorize(opened.session, "main", expired[0].2, Access::Write).is_ok());
        assert!(docs.authorize(opened.session, "doc-1", accepted.lease, Access::Read).is_err());
    }

    #[test]
    fn accept_does_not_wait_for_pre_seal_file_lock() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h
1".to_vec(), false).unwrap(), "main", None);
        let cloned = docs.authorize(opened.session, "main", opened.lease, Access::Write).unwrap();
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let guard = cloned.file.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let accepted = docs.accept(ticket, "doc-1").map(|adopted| adopted.lease);
            tx.send(accepted).unwrap();
        });
        let outcome = rx.recv_timeout(Duration::from_secs(2));
        drop(guard);
        worker.join().unwrap();
        let lease = outcome.expect("accept 不得等待 seal 前克隆命令的文件锁").unwrap();
        assert!(lease > opened.lease);
    }

    #[test]
    fn cloned_entry_is_revalidated_after_file_lock() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let cloned = docs.authorize(opened.session, "main", opened.lease, Access::Write).unwrap();
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let adopted = docs.accept(ticket, "doc-1").unwrap();
        let _file = cloned.file.lock().unwrap();
        assert!(docs.authorize(opened.session, "main", opened.lease, Access::Write).is_err());
        assert!(docs.authorize(opened.session, "doc-1", adopted.lease, Access::Write).is_err());
    }

    #[test]
    fn target_destroy_after_accept_cancels_to_live_source() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let accepted = docs.accept(ticket, "doc-1").unwrap();
        let live = std::collections::HashSet::from(["main".to_string()]);
        let outcomes = docs.destroy_window("doc-1", &live, Some("main"));
        let [DestroyOutcome::Cancelled { owner, lease, .. }] = outcomes.as_slice() else {
            panic!("cancel expected");
        };
        assert_eq!(owner, "main");
        assert!(*lease > accepted.lease);
        assert!(docs.authorize(opened.session, "main", *lease, Access::Write).is_ok());
    }

    #[test]
    fn source_destroy_after_adopt_completes_to_live_target() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let accepted = docs.accept(ticket, "doc-1").unwrap();
        docs.mark_adopted(ticket, "doc-1").unwrap();
        let live = std::collections::HashSet::from(["doc-1".to_string()]);
        let outcomes = docs.destroy_window("main", &live, Some("doc-1"));
        assert!(matches!(outcomes.as_slice(), [DestroyOutcome::Completed { .. }]));
        assert!(docs.authorize(opened.session, "doc-1", accepted.lease, Access::Write).is_ok());
    }

    #[test]
    fn source_destroy_before_adopt_defers_complete_until_target_adopts() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let accepted = docs.accept(ticket, "doc-1").unwrap();
        let live = std::collections::HashSet::from(["doc-1".to_string()]);
        let outcomes = docs.destroy_window("main", &live, Some("doc-1"));
        assert!(matches!(outcomes.as_slice(), [DestroyOutcome::Detached { .. }]));
        assert!(docs.authorize(opened.session, "doc-1", accepted.lease, Access::Write).is_err());
        let notice = docs.mark_adopted(ticket, "doc-1").unwrap();
        assert!(!notice.source_live);
        assert!(docs.authorize(opened.session, "doc-1", notice.lease, Access::Write).is_ok());
    }

    #[test]
    fn sealed_source_destroy_recovers_idle_session_on_target() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let _ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        let live = std::collections::HashSet::from(["doc-1".to_string()]);
        let outcomes = docs.destroy_window("main", &live, Some("doc-1"));
        let [DestroyOutcome::Recovered { owner, lease, .. }] = outcomes.as_slice() else {
            panic!("recovery expected");
        };
        assert_eq!(owner, "doc-1");
        assert!(docs.authorize(opened.session, "doc-1", *lease, Access::Write).is_ok());
    }

    #[test]
    fn destroy_without_live_owner_defers_to_process_cleanup() {
        let mut docs = DocumentRegistry::default();
        let opened = docs.insert_new(BigFile::from_bytes(b"h\n1".to_vec(), false).unwrap(), "main", None);
        let ticket = docs.seal(opened.session, "main", opened.lease, "doc-1", "A", 0, serde_json::json!({})).unwrap();
        docs.accept(ticket, "doc-1").unwrap();
        let outcomes = docs.destroy_window("doc-1", &std::collections::HashSet::new(), None);
        assert!(matches!(outcomes.as_slice(), [DestroyOutcome::Dropped { .. }]));
    }

}
