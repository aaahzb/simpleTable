use crate::history::{Cost, HistoryError, SharedCharge};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const MEM_SNAPSHOT_MAX_BYTES: u64 = 1024 * 1024;
pub const SPOOL_RESERVE_BYTES: u64 = 8 * 1024 * 1024;

/// 当前文档会话独占的临时目录及文件编号。
pub struct SessionSpool {
    root: PathBuf,
    owned: bool,
    next_file: AtomicU64,
}

impl SessionSpool {
    /// 在全新路径创建独占目录，并验证写权限。
    pub fn create(root: &Path) -> Result<Self, String> {
        if root.exists() {
            return Err(format!("spool 路径已存在：{}", root.display()));
        }
        fs::create_dir(root).map_err(|e| format!("创建 spool 失败：{e}"))?;
        let spool = Self {
            root: root.to_path_buf(),
            owned: true,
            next_file: AtomicU64::new(0),
        };
        let probe = root.join("write-probe");
        File::create(&probe).map_err(|e| format!("spool 不可写：{e}"))?;
        fs::remove_file(&probe).map_err(|e| format!("清理 spool 探针失败：{e}"))?;
        Ok(spool)
    }

    pub fn root(&self) -> &Path { &self.root }

    /// 写入前检查临时卷空间，并保留 8 MiB 余量。
    pub fn ensure_capacity(&self, required: u64) -> Result<(), String> {
        let needed = required.checked_add(SPOOL_RESERVE_BYTES).ok_or("spool 空间需求溢出")?;
        let available = fs2::available_space(&self.root)
            .map_err(|e| format!("读取 spool 可用空间失败：{e}"))?;
        if available < needed {
            return Err(format!("临时空间不足：需要 {needed} 字节，可用 {available} 字节"));
        }
        Ok(())
    }

    /// 原子创建会话内唯一文件。
    pub fn new_file(&self, kind: &str) -> Result<PathBuf, String> {
        if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("spool 文件种类只能含 ASCII 字母、数字和连字符".into());
        }
        let id = self.next_file.fetch_add(1, Ordering::Relaxed);
        let path = self.root.join(format!("{id:016x}-{kind}.tmp"));
        OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| format!("创建 spool 文件失败：{e}"))?;
        Ok(path)
    }
}

impl Drop for SessionSpool {
    fn drop(&mut self) {
        if self.owned {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// 自包含删除行内容；小快照留内存，大快照落会话临时目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowSnapshot {
    Memory(Arc<MemorySnapshot>),
    Disk(Arc<DiskSnapshot>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct MemorySnapshot {
    rows: Vec<Vec<String>>,
    history_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiskSnapshot {
    path: PathBuf,
    index: Vec<SnapshotIndex>,
    logical_bytes: u64,
    history_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SnapshotIndex {
    pub offset: u64,
    pub length: u64,
}

fn memory_history_bytes(rows: &Vec<Vec<String>>) -> Result<u64, String> {
    let mut cost = Cost::new();
    cost.add_value::<MemorySnapshot>().map_err(|e| format!("{e:?}"))?;
    cost.add((4 * std::mem::size_of::<usize>()) as u64).map_err(|e| format!("{e:?}"))?;
    cost.add_vec(rows).map_err(|e| format!("{e:?}"))?;
    for row in rows {
        cost.add_vec(row).map_err(|e| format!("{e:?}"))?;
        for value in row {
            cost.add_string(value).map_err(|e| format!("{e:?}"))?;
        }
    }
    Ok(cost.bytes())
}

fn disk_history_bytes(path: &Path, index: &Vec<SnapshotIndex>, logical: u64)
    -> Result<u64, String> {
    let mut cost = Cost::new();
    cost.add_value::<DiskSnapshot>().map_err(|e| format!("{e:?}"))?;
    cost.add((4 * std::mem::size_of::<usize>()) as u64).map_err(|e| format!("{e:?}"))?;
    cost.add(logical).map_err(|e| format!("{e:?}"))?;
    cost.add_vec(index).map_err(|e| format!("{e:?}"))?;
    cost.add(path.as_os_str().len() as u64).map_err(|e| format!("{e:?}"))?;
    Ok(cost.bytes())
}

fn write_disk_row(
    file: &mut File,
    row: &[String],
    offset: &mut u64,
    index: &mut Vec<SnapshotIndex>,
) -> Result<(), String> {
    let cols = u32::try_from(row.len()).map_err(|_| "快照列数超过 u32")?;
    let mut record_bytes = 4u64;
    for value in row {
        record_bytes = record_bytes.checked_add(8)
            .and_then(|n| n.checked_add(value.len() as u64))
            .ok_or("快照行长度溢出")?;
    }
    let start = *offset;
    *offset = offset.checked_add(record_bytes).ok_or("快照偏移溢出")?;
    file.write_all(&cols.to_le_bytes()).map_err(|e| e.to_string())?;
    for value in row {
        file.write_all(&(value.len() as u64).to_le_bytes()).map_err(|e| e.to_string())?;
        file.write_all(value.as_bytes()).map_err(|e| e.to_string())?;
    }
    index.push(SnapshotIndex { offset: start, length: record_bytes });
    Ok(())
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = pos.checked_add(len).ok_or("快照读取偏移溢出")?;
    if end > buf.len() {
        return Err("快照记录截断".into());
    }
    let out = &buf[*pos..end];
    *pos = end;
    Ok(out)
}

impl RowSnapshot {
    /// 将完整逻辑行写入内存或会话临时文件，并预核算历史成本。
    pub fn write<I>(spool: &SessionSpool, rows: I) -> Result<Self, String>
    where
        I: Iterator<Item = Vec<String>>,
    {
        let mut buffered = Vec::<Vec<String>>::new();
        let mut logical_bytes = 0u64;
        let mut disk: Option<(PathBuf, File, Vec<SnapshotIndex>, u64)> = None;
        for row in rows {
            for value in &row {
                logical_bytes = logical_bytes.checked_add(value.len() as u64)
                    .ok_or("快照逻辑内容长度溢出")?;
            }
            if disk.is_none() && logical_bytes <= MEM_SNAPSHOT_MAX_BYTES {
                buffered.push(row);
                continue;
            }
            if disk.is_none() {
                let path = spool.new_file("rows")?;
                spool.ensure_capacity(logical_bytes)?;
                let mut file = OpenOptions::new().write(true).open(&path)
                    .map_err(|e| e.to_string())?;
                file.write_all(b"STSP").and_then(|_| file.write_all(&0u64.to_le_bytes()))
                    .map_err(|e| e.to_string())?;
                let mut index = Vec::new();
                let mut offset = 12u64;
                for old in buffered.drain(..) {
                    write_disk_row(&mut file, &old, &mut offset, &mut index)?;
                }
                disk = Some((path, file, index, offset));
            }
            let (_, file, index, offset) = disk.as_mut().unwrap();
            write_disk_row(file, &row, offset, index)?;
        }
        if let Some((path, mut file, index, _)) = disk {
            let count = u64::try_from(index.len()).map_err(|_| "快照行数超过 u64")?;
            file.seek(SeekFrom::Start(4)).and_then(|_| file.write_all(&count.to_le_bytes()))
                .map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            let history_bytes = disk_history_bytes(&path, &index, logical_bytes)?;
            return Ok(Self::Disk(Arc::new(DiskSnapshot {
                path,
                index,
                logical_bytes,
                history_bytes,
            })));
        }
        let history_bytes = memory_history_bytes(&buffered)?;
        Ok(Self::Memory(Arc::new(MemorySnapshot { rows: buffered, history_bytes })))
    }

    /// 按快照内行号读取完整逻辑行。
    pub fn read_row(&self, row: usize) -> Result<Vec<String>, String> {
        match self {
            Self::Memory(value) => value.rows.get(row).cloned().ok_or("快照行越界".into()),
            Self::Disk(value) => {
                let item = value.index.get(row).ok_or("快照行越界")?;
                let mut file = File::open(&value.path).map_err(|e| e.to_string())?;
                let mut header = [0u8; 12];
                file.read_exact(&mut header).map_err(|e| e.to_string())?;
                if &header[..4] != b"STSP" {
                    return Err("快照 magic 错误".into());
                }
                let count = u64::from_le_bytes(header[4..12].try_into().unwrap());
                if count != value.index.len() as u64 {
                    return Err("快照记录数不一致".into());
                }
                file.seek(SeekFrom::Start(item.offset)).map_err(|e| e.to_string())?;
                let len = usize::try_from(item.length).map_err(|_| "快照行超出内存地址范围")?;
                let mut buf = vec![0u8; len];
                file.read_exact(&mut buf).map_err(|e| e.to_string())?;
                let mut pos = 0usize;
                let cols = u32::from_le_bytes(take(&buf, &mut pos, 4)?.try_into().unwrap());
                let mut out = Vec::with_capacity(cols as usize);
                for _ in 0..cols {
                    let len = u64::from_le_bytes(take(&buf, &mut pos, 8)?.try_into().unwrap());
                    let len = usize::try_from(len).map_err(|_| "快照字段超出地址范围")?;
                    out.push(String::from_utf8(take(&buf, &mut pos, len)?.to_vec())
                        .map_err(|_| "快照字段不是 UTF-8")?);
                }
                if pos != buf.len() {
                    return Err("快照记录存在多余字节".into());
                }
                Ok(out)
            }
        }
    }

    pub fn history_bytes(&self) -> u64 {
        match self {
            Self::Memory(value) => value.history_bytes,
            Self::Disk(value) => value.history_bytes,
        }
    }

    pub fn shared_charge(&self) -> SharedCharge {
        match self {
            Self::Memory(value) => SharedCharge::of(value, self.history_bytes()),
            Self::Disk(value) => SharedCharge::of(value, self.history_bytes()),
        }
    }
}

/// 逻辑 UTF-8 内容转换为历史账目。
pub fn history_text_bytes(utf8_len: u64) -> Result<u64, HistoryError> {
    utf8_len.checked_add((4 * std::mem::size_of::<usize>()) as u64)
        .ok_or(HistoryError::CostOverflow)
}

#[cfg(test)]
mod tests {
    use super::{RowSnapshot, SessionSpool, MEM_SNAPSHOT_MAX_BYTES, history_text_bytes};
    use crate::history::HistoryError;

    #[test]
    fn spool_snapshot_roundtrip_and_drop_cleans_only_owned_dir() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "simpletable-spool-test-{}-{nonce}", std::process::id()));
        let spool = SessionSpool::create(&root).unwrap();
        let owned = spool.root().to_path_buf();
        let rows = vec![vec!["a".to_string(), "中文".to_string()], vec!["b".to_string()]];
        let snapshot = RowSnapshot::write(&spool, rows.clone().into_iter()).unwrap();
        assert_eq!(snapshot.read_row(0).unwrap(), rows[0]);
        assert_eq!(snapshot.read_row(1).unwrap(), rows[1]);
        assert!(snapshot.history_bytes() >= "a中文b".len() as u64);
        assert_eq!(snapshot.shared_charge(), snapshot.clone().shared_charge());
        drop(snapshot);
        drop(spool);
        assert!(!owned.exists());
    }

    #[test]
    fn large_snapshot_spills_to_disk_without_losing_logical_charge() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "simpletable-large-spool-{}-{nonce}", std::process::id()));
        let spool = SessionSpool::create(&root).unwrap();
        let rows = vec![vec!["x".repeat(MEM_SNAPSHOT_MAX_BYTES as usize + 1)]];
        let snapshot = RowSnapshot::write(&spool, rows.into_iter()).unwrap();
        assert!(matches!(snapshot, RowSnapshot::Disk(_)));
        assert!(snapshot.history_bytes() > MEM_SNAPSHOT_MAX_BYTES);
        drop(snapshot);
        drop(spool);
        assert!(!root.exists());
    }

    #[test]
    fn history_text_bytes_is_checked() {
        assert_eq!(history_text_bytes(7), Ok(7 + 4 * std::mem::size_of::<usize>() as u64));
        assert_eq!(history_text_bytes(u64::MAX), Err(HistoryError::CostOverflow));
    }

    #[test]
    fn existing_spool_root_is_rejected_without_deleting_content() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "simpletable-existing-spool-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let marker = root.join("keep.txt");
        std::fs::write(&marker, b"keep").unwrap();
        assert!(SessionSpool::create(&root).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep");
        std::fs::remove_file(&marker).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
}
