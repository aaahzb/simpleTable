use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

pub type RequestId = u64;
pub type BatchId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PathKey {
    Unicode(String),
    Opaque(PathBuf),
}

impl PathKey {
    pub fn new(path: &Path) -> Self {
        let normalized = path.canonicalize().unwrap_or_else(|_| lexical_normalize(path));
        match normalized.to_str() {
            #[cfg(windows)]
            Some(text) => Self::Unicode(text.to_lowercase()),
            #[cfg(not(windows))]
            Some(text) => Self::Unicode(text.to_string()),
            None => Self::Opaque(normalized),
        }
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut normal_depth = 0usize;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                out.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if normal_depth > 0 {
                    out.pop();
                    normal_depth -= 1;
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            Component::Normal(part) => {
                out.push(part);
                normal_depth += 1;
            }
        }
    }
    out
}

pub fn normalize_args(args: Vec<OsString>, cwd: &Path) -> Vec<PathBuf> {
    args.into_iter()
        .skip(1)
        .filter_map(|arg| {
            let path = PathBuf::from(arg);
            if path.to_str().is_some_and(|text| text.starts_with('-')) {
                return None;
            }
            let is_csv = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"));
            if !is_csv {
                return None;
            }
            let absolute = if path.is_absolute() { path } else { cwd.join(path) };
            Some(lexical_normalize(&absolute))
        })
        .collect()
}

pub fn file_url_to_path(url: &str) -> Result<PathBuf, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("无效的文件 URL：{e}"))?;
    if parsed.scheme() != "file" {
        return Err("仅支持本地 file URL".to_string());
    }
    parsed.to_file_path().map_err(|_| "无法将文件 URL 转换为本地路径".to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum OpenItem {
    Open { request_id: RequestId, path: PathBuf },
    Activate { path: PathBuf, session: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenBatch {
    pub batch_id: BatchId,
    pub target: String,
    pub items: Vec<OpenItem>,
}

#[derive(Clone, Debug)]
struct PendingOpen {
    request_id: RequestId,
    target: String,
    waiters: usize,
}

#[derive(Clone, Debug)]
struct Opened {
    session: u64,
    owner: String,
    path: PathBuf,
    transferring: bool,
    waiters: usize,
}

#[derive(Clone, Debug)]
struct SaveReservation {
    session: u64,
    owner: String,
}

#[derive(Default)]
pub struct OpenRequestQueue {
    next_request: RequestId,
    next_batch: BatchId,
    ready: HashSet<String>,
    pending: HashMap<String, VecDeque<OpenBatch>>,
    in_flight: HashMap<String, OpenBatch>,
    reservations: HashMap<PathKey, PendingOpen>,
    save_reservations: HashMap<PathKey, SaveReservation>,
    opened: HashMap<PathKey, Opened>,
}

impl OpenRequestQueue {
    fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        self.next_request
    }

    fn batch_id(&mut self) -> BatchId {
        self.next_batch += 1;
        self.next_batch
    }

    fn push(&mut self, target: String, items: Vec<OpenItem>) -> Option<OpenBatch> {
        if items.is_empty() {
            return None;
        }
        let batch = OpenBatch { batch_id: self.batch_id(), target: target.clone(), items };
        if self.ready.contains(&target) && !self.in_flight.contains_key(&target) {
            self.in_flight.insert(target, batch.clone());
            Some(batch)
        } else {
            self.pending.entry(target).or_default().push_back(batch);
            None
        }
    }

    fn take_next(&mut self, target: &str) -> Option<OpenBatch> {
        if !self.ready.contains(target) || self.in_flight.contains_key(target) {
            return None;
        }
        let batch = self.pending.get_mut(target)?.pop_front();
        if self.pending.get(target).is_some_and(VecDeque::is_empty) {
            self.pending.remove(target);
        }
        if let Some(batch) = batch.as_ref() {
            self.in_flight.insert(target.to_string(), batch.clone());
        }
        batch
    }

    pub fn enqueue(&mut self, paths: Vec<PathBuf>, recent: &str) -> Vec<OpenBatch> {
        let mut seen = HashSet::new();
        let mut groups: Vec<(String, Vec<OpenItem>)> = Vec::new();
        for path in paths {
            let key = PathKey::new(&path);
            if !seen.insert(key.clone()) {
                continue;
            }
            let delivery = if let Some(opened) = self.opened.get_mut(&key) {
                if opened.transferring {
                    opened.waiters += 1;
                    None
                } else {
                    Some((opened.owner.clone(), OpenItem::Activate {
                        path: opened.path.clone(), session: opened.session,
                    }))
                }
            } else if let Some(pending) = self.reservations.get_mut(&key) {
                pending.waiters += 1;
                None
            } else {
                let request_id = self.request_id();
                self.reservations.insert(key, PendingOpen {
                    request_id, target: recent.to_string(), waiters: 0,
                });
                Some((recent.to_string(), OpenItem::Open { request_id, path }))
            };
            if let Some((target, item)) = delivery {
                if let Some(index) = groups.iter().position(|(label, _)| label == &target) {
                    groups[index].1.push(item);
                } else {
                    groups.push((target, vec![item]));
                }
            }
        }
        groups.into_iter().filter_map(|(target, items)| self.push(target, items)).collect()
    }

    pub fn mark_ready(&mut self, target: &str) -> Option<OpenBatch> {
        self.ready.insert(target.to_string());
        self.take_next(target)
    }

    pub fn ack(&mut self, target: &str, batch_id: BatchId) -> Result<Option<OpenBatch>, String> {
        let matches = self.in_flight.get(target)
            .is_some_and(|batch| batch.batch_id == batch_id);
        if !matches {
            return Err("打开批次回执不匹配".to_string());
        }
        self.in_flight.remove(target);
        Ok(self.take_next(target))
    }

    pub fn complete_open(
        &mut self,
        request_id: RequestId,
        path: &Path,
        session: u64,
        owner: &str,
    ) -> Result<Vec<OpenBatch>, String> {
        let key = PathKey::new(path);
        let pending = self.reservations.get(&key)
            .filter(|pending| pending.request_id == request_id)
            .ok_or_else(|| "打开请求预留不存在或已失效".to_string())?;
        if self.opened.get(&key).is_some_and(|opened| opened.session != session) {
            return Err("目标文件已在其他标签中打开".to_string());
        }
        let waiters = pending.waiters;
        self.reservations.remove(&key);
        self.opened.insert(key, Opened {
            session,
            owner: owner.to_string(),
            path: path.to_path_buf(),
            transferring: false,
            waiters: 0,
        });
        if waiters == 0 {
            return Ok(Vec::new());
        }
        Ok(self.push(owner.to_string(), vec![OpenItem::Activate {
            path: path.to_path_buf(), session,
        }]).into_iter().collect())
    }

    pub fn fail_open(
        &mut self,
        request_id: RequestId,
        path: &Path,
        recent: &str,
    ) -> Result<Vec<OpenBatch>, String> {
        let key = PathKey::new(path);
        let pending = self.reservations.get(&key)
            .filter(|pending| pending.request_id == request_id)
            .ok_or_else(|| "打开请求预留不存在或已失效".to_string())?;
        let retry = pending.waiters > 0;
        self.reservations.remove(&key);
        if !retry {
            return Ok(Vec::new());
        }
        let next_request = self.request_id();
        self.reservations.insert(key, PendingOpen {
            request_id: next_request, target: recent.to_string(), waiters: 0,
        });
        Ok(self.push(recent.to_string(), vec![OpenItem::Open {
            request_id: next_request, path: path.to_path_buf(),
        }]).into_iter().collect())
    }

    pub fn remove_window(&mut self, label: &str, recent: &str) -> Vec<OpenBatch> {
        self.ready.remove(label);
        let mut batches = Vec::new();
        if let Some(batch) = self.in_flight.remove(label) {
            batches.push(batch);
        }
        if let Some(pending) = self.pending.remove(label) {
            batches.extend(pending);
        }
        let mut delivered = Vec::new();
        for batch in batches {
            for item in batch.items {
                match item {
                    OpenItem::Open { request_id, path } => {
                        let key = PathKey::new(&path);
                        let valid = self.reservations.get_mut(&key).is_some_and(|pending| {
                            if pending.request_id != request_id {
                                return false;
                            }
                            pending.target = recent.to_string();
                            true
                        });
                        if valid {
                            delivered.extend(self.push(recent.to_string(), vec![
                                OpenItem::Open { request_id, path },
                            ]));
                        }
                    }
                    OpenItem::Activate { path, session } => {
                        let opened = self.opened.get(&PathKey::new(&path))
                            .filter(|opened| opened.session == session && opened.owner != label);
                        if let Some(opened) = opened {
                            let target = opened.owner.clone();
                            let path = opened.path.clone();
                            delivered.extend(self.push(target, vec![
                                OpenItem::Activate { path, session },
                            ]));
                        }
                    }
                }
            }
        }
        delivered
    }

    pub fn reserve_save_path(
        &mut self,
        session: u64,
        path: &Path,
        owner: &str,
    ) -> Result<(), String> {
        let key = PathKey::new(path);
        if let Some(opened) = self.opened.get(&key) {
            if opened.session != session {
                return Err("目标文件已在其他标签中打开".to_string());
            }
            if opened.owner != owner {
                return Err("文档窗口所有权已变化".to_string());
            }
        }
        if self.reservations.contains_key(&key) {
            return Err("目标文件正在等待打开".to_string());
        }
        if self.opened.values().any(|opened| {
            opened.session == session && opened.owner != owner
        }) {
            return Err("文档窗口所有权已变化".to_string());
        }
        if let Some(reserved) = self.save_reservations.get(&key) {
            if reserved.session != session || reserved.owner != owner {
                return Err("目标文件正在被其他会话保存".to_string());
            }
            return Ok(());
        }
        self.save_reservations.insert(key, SaveReservation {
            session, owner: owner.to_string(),
        });
        Ok(())
    }

    pub fn bind_session_path(
        &mut self,
        session: u64,
        path: &Path,
        owner: &str,
    ) -> Result<(), String> {
        let key = PathKey::new(path);
        self.save_reservations.get(&key)
            .filter(|saved| saved.session == session && saved.owner == owner)
            .ok_or_else(|| "保存路径预留不存在或已失效".to_string())?;
        if self.opened.get(&key).is_some_and(|opened| {
            opened.session != session || opened.owner != owner
        }) {
            return Err("目标文件已在其他标签中打开".to_string());
        }
        if self.opened.values().any(|opened| {
            opened.session == session && opened.owner != owner
        }) {
            return Err("文档窗口所有权已变化".to_string());
        }
        let old_keys: Vec<PathKey> = self.opened.iter()
            .filter(|(_, opened)| opened.session == session)
            .map(|(old, _)| old.clone())
            .collect();
        let state = self.opened.get(&key).or_else(|| {
            old_keys.first().and_then(|old| self.opened.get(old))
        });
        let (transferring, waiters) = state
            .map(|opened| (opened.transferring, opened.waiters))
            .unwrap_or((false, 0));
        for old in old_keys {
            self.opened.remove(&old);
        }
        self.opened.insert(key.clone(), Opened {
            session,
            owner: owner.to_string(),
            path: path.to_path_buf(),
            transferring,
            waiters,
        });
        self.save_reservations.remove(&key);
        Ok(())
    }

    pub fn release_save_reservation(&mut self, session: u64, path: &Path) {
        let key = PathKey::new(path);
        if self.save_reservations.get(&key).is_some_and(|saved| saved.session == session) {
            self.save_reservations.remove(&key);
        }
    }

    pub fn remove_session(&mut self, session: u64) {
        let removed: HashSet<PathKey> = self.opened.iter()
            .filter(|(_, opened)| opened.session == session)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &removed {
            self.opened.remove(key);
        }
        self.save_reservations.retain(|_, saved| saved.session != session);
        for queue in self.pending.values_mut() {
            for batch in queue.iter_mut() {
                batch.items.retain(|item| match item {
                    OpenItem::Activate { path, .. } => !removed.contains(&PathKey::new(path)),
                    OpenItem::Open { .. } => true,
                });
            }
            queue.retain(|batch| !batch.items.is_empty());
        }
        self.pending.retain(|_, queue| !queue.is_empty());
    }

    pub fn set_session_owner(&mut self, session: u64, owner: &str) {
        for opened in self.opened.values_mut() {
            if opened.session == session {
                opened.owner = owner.to_string();
            }
        }
    }

    pub fn set_transferring(&mut self, session: u64, transferring: bool) -> Vec<OpenBatch> {
        let mut activations = Vec::new();
        for opened in self.opened.values_mut() {
            if opened.session != session {
                continue;
            }
            opened.transferring = transferring;
            if !transferring && opened.waiters > 0 {
                opened.waiters = 0;
                activations.push((opened.owner.clone(), OpenItem::Activate {
                    path: opened.path.clone(), session,
                }));
            }
        }
        activations.into_iter().filter_map(|(target, item)| {
            self.push(target, vec![item])
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn open_one(q: &mut OpenRequestQueue, path: &str, session: u64, owner: &str) {
        q.mark_ready(owner);
        let batch = q.enqueue(vec![PathBuf::from(path)], owner).remove(0);
        let OpenItem::Open { request_id, .. } = batch.items[0] else {
            panic!("open expected");
        };
        q.ack(owner, batch.batch_id).unwrap();
        q.complete_open(request_id, Path::new(path), session, owner).unwrap();
    }

    #[test]
    fn args_resolve_csv_against_each_invocation_cwd() {
        let cwd = Path::new(if cfg!(windows) { r"C:\work" } else { "/work" });
        let got = normalize_args(
            vec!["simpleTable.exe".into(), "--smoke".into(), "a.CSV".into(), "x.txt".into()],
            cwd,
        );
        assert_eq!(got, vec![cwd.join("a.CSV")]);
    }

    #[test]
    fn ready_take_ack_is_serial_and_delivery_is_single_consumer() {
        let mut q = OpenRequestQueue::default();
        assert!(q.enqueue(vec![PathBuf::from("a.csv"), PathBuf::from("b.csv")], "main").is_empty());
        let first = q.mark_ready("main").expect("first batch");
        assert_eq!(first.items.len(), 2);
        assert!(q.enqueue(vec![PathBuf::from("c.csv")], "main").is_empty());
        assert!(q.mark_ready("main").is_none(), "in-flight batch cannot be taken twice");
        let second = q.ack("main", first.batch_id).unwrap().expect("second batch");
        assert_eq!(second.items.len(), 1);
        assert!(q.ack("main", second.batch_id).unwrap().is_none());
    }

    #[test]
    fn duplicate_pending_path_opens_once_then_activates_owner() {
        let mut q = OpenRequestQueue::default();
        q.mark_ready("main");
        let batch = q.enqueue(vec![PathBuf::from("a.csv")], "main").remove(0);
        let OpenItem::Open { request_id, .. } = batch.items[0] else { panic!("open expected") };
        assert!(q.enqueue(vec![PathBuf::from("a.csv")], "main").is_empty());
        q.ack("main", batch.batch_id).unwrap();
        let delivered = q.complete_open(request_id, Path::new("a.csv"), 7, "main").unwrap();
        assert!(matches!(delivered[0].items[0], OpenItem::Activate { session: 7, .. }));
    }

    #[test]
    fn failed_open_requeues_waiter_and_destroyed_window_requeues_inflight() {
        let mut q = OpenRequestQueue::default();
        q.mark_ready("main");
        let first = q.enqueue(vec![PathBuf::from("a.csv")], "main").remove(0);
        q.enqueue(vec![PathBuf::from("a.csv")], "main");
        let OpenItem::Open { request_id, .. } = first.items[0] else { panic!("open expected") };
        q.mark_ready("doc-1");
        let retried = q.fail_open(request_id, Path::new("a.csv"), "doc-1").unwrap();
        assert_eq!(retried[0].target, "doc-1");
        assert!(q.ack("main", first.batch_id).unwrap().is_none());
        let moved = q.remove_window("doc-1", "main");
        assert!(moved.iter().all(|b| b.target == "main"));
    }

    #[test]
    fn open_batches_serialize_kind_and_camel_case_fields() {
        let batch = OpenBatch {
            batch_id: 3,
            target: "main".into(),
            items: vec![OpenItem::Open { request_id: 7, path: PathBuf::from("a.csv") }],
        };
        assert_eq!(serde_json::to_value(batch).unwrap(), serde_json::json!({
            "batchId": 3, "target": "main",
            "items": [{"kind": "open", "requestId": 7, "path": "a.csv"}]
        }));
    }

    #[test]
    fn macos_file_url_must_be_local_file() {
        let expected = if cfg!(windows) { "C:/tmp/a.csv" } else { "/tmp/a.csv" };
        let url = url::Url::from_file_path(expected).unwrap();
        assert_eq!(file_url_to_path(url.as_str()).unwrap(), PathBuf::from(expected));
        assert!(file_url_to_path("https://example.invalid/a.csv").is_err());
    }

    #[test]
    fn save_reservation_conflicts_are_atomic_and_bind_releases_old_path() {
        let mut q = OpenRequestQueue::default();
        open_one(&mut q, "a.csv", 1, "main");
        open_one(&mut q, "b.csv", 2, "main");
        assert!(q.reserve_save_path(1, Path::new("b.csv"), "main").is_err());
        q.reserve_save_path(1, Path::new("new.csv"), "main").unwrap();
        assert!(q.reserve_save_path(2, Path::new("new.csv"), "main").is_err());
        assert!(q.bind_session_path(1, Path::new("new.csv"), "other").is_err());
        assert!(q.reserve_save_path(2, Path::new("new.csv"), "main").is_err());
        q.bind_session_path(1, Path::new("new.csv"), "main").unwrap();
        q.mark_ready("main");
        let rebound = q.enqueue(vec![PathBuf::from("new.csv")], "main").remove(0);
        assert!(matches!(rebound.items[0], OpenItem::Activate { session: 1, .. }));
        q.ack("main", rebound.batch_id).unwrap();
        let reopened = q.enqueue(vec![PathBuf::from("a.csv")], "main").remove(0);
        assert!(matches!(reopened.items[0], OpenItem::Open { .. }));
    }

    #[test]
    fn transferring_defers_activation_until_restored_owner() {
        let mut q = OpenRequestQueue::default();
        open_one(&mut q, "a.csv", 9, "main");
        assert!(q.set_transferring(9, true).is_empty());
        assert!(q.enqueue(vec![PathBuf::from("a.csv")], "doc-1").is_empty());
        q.set_session_owner(9, "doc-1");
        assert!(q.set_transferring(9, false).is_empty());
        let delivered = vec![q.mark_ready("doc-1").expect("ready activation")];
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].target, "doc-1");
        assert!(matches!(delivered[0].items[0], OpenItem::Activate { session: 9, .. }));
        assert!(q.set_transferring(9, false).is_empty());
    }
}
