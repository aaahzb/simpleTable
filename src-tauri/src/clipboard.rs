//! 进程级应用内剪贴板：小结果留内存，大结果流式写入独占临时目录。

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::spool::SPOOL_RESERVE_BYTES;

pub const LARGE_COPY_BYTES: u64 = 8 * 1024 * 1024;
pub const CLIPBOARD_FORMAT_VERSION: u32 = 1;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardMeta {
    pub token: String,
    pub rows: u64,
    pub cols: u32,
    pub bytes: u64,
    pub version: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CopyOutcome {
    Text { text: String, bytes: u64 },
    App { meta: ClipboardMeta },
}

pub struct ClipboardEntry {
    pub token: String,
    pub path: PathBuf,
    pub rows: u64,
    pub cols: u32,
    pub bytes: u64,
}

impl ClipboardEntry {
    fn meta(&self) -> ClipboardMeta {
        ClipboardMeta {
            token: self.token.clone(),
            rows: self.rows,
            cols: self.cols,
            bytes: self.bytes,
            version: CLIPBOARD_FORMAT_VERSION,
        }
    }
}

impl Drop for ClipboardEntry {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct ClipboardStore {
    root: PathBuf,
    current: Mutex<Option<Arc<ClipboardEntry>>>,
    next_token: AtomicU64,
}

fn ensure_capacity(root: &Path, required: u64) -> Result<(), String> {
    let needed = required.checked_add(SPOOL_RESERVE_BYTES).ok_or("剪贴板空间需求溢出")?;
    let available = fs2::available_space(root).map_err(|e| format!("读取剪贴板可用空间失败：{e}"))?;
    if available < needed {
        return Err(format!("临时空间不足：需要 {needed} 字节，可用 {available} 字节"));
    }
    Ok(())
}

impl ClipboardStore {
    /// 仅供退出冒烟报告探针使用；清理仍由 ClipboardStore 自己执行。
    pub fn root(&self) -> &Path { &self.root }

    /// 创建本进程独占的临时目录，并验证空间与写权限。
    pub fn create_unique() -> Result<Self, String> {
        let temp = std::env::temp_dir();
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|e| format!("读取系统时间失败：{e}"))?.as_nanos();
        for attempt in 0..100u32 {
            let root = temp.join(format!(
                "simpletable-clipboard-{}-{nonce}-{attempt}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let validation = (|| {
                        ensure_capacity(&root, 0)?;
                        let probe = root.join("write-probe");
                        File::create(&probe).map_err(|e| format!("剪贴板目录不可写：{e}"))?;
                        fs::remove_file(&probe).map_err(|e| format!("清理剪贴板探针失败：{e}"))?;
                        Ok(())
                    })();
                    if let Err(e) = validation {
                        let _ = fs::remove_dir_all(&root);
                        return Err(e);
                    }
                    return Ok(Self {
                        root,
                        current: Mutex::new(None),
                        next_token: AtomicU64::new(0),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("创建剪贴板临时目录失败：{e}")),
            }
        }
        Err("无法创建唯一的剪贴板临时目录".into())
    }

    fn new_temp_file(&self, required: u64) -> Result<(String, PathBuf, BufWriter<File>), String> {
        ensure_capacity(&self.root, required)?;
        let id = self.next_token.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{id}", std::process::id());
        let path = self.root.join(format!("{token}.tmp"));
        let file = OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| format!("创建剪贴板临时文件失败：{e}"))?;
        Ok((token, path, BufWriter::with_capacity(COPY_BUFFER_BYTES, file)))
    }

    fn publish(&self, token: String, path: PathBuf, rows: u64, cols: u32, bytes: u64)
        -> ClipboardMeta {
        let entry = ClipboardEntry { token, path, rows, cols, bytes };
        let meta = entry.meta();
        let old = {
            let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
            current.replace(Arc::new(entry))
        };
        // 旧 Arc 读者仍持有文件；只有最后一个读者释放时才删除。
        drop(old);
        meta
    }

    pub fn get(&self, token: &str) -> Option<Arc<ClipboardEntry>> {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        match current.as_ref() {
            Some(entry) if entry.token == token => Some(entry.clone()),
            _ => None,
        }
    }

    pub fn current_meta(&self) -> Option<ClipboardMeta> {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        current.as_ref().map(|entry| entry.meta())
    }

    pub fn clear_current(&self) {
        let old = {
            let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
            current.take()
        };
        drop(old);
    }

    pub fn cleanup(&self) {
        self.clear_current();
        let _ = fs::remove_dir_all(&self.root);
    }

    #[cfg(test)]
    pub fn put_test_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        let (token, temp, mut writer) = self.new_temp_file(bytes.len() as u64)?;
        let final_path = self.root.join(format!("{token}.tsv"));
        let result = (|| {
            writer.write_all(bytes).map_err(|e| format!("写测试剪贴板失败：{e}"))?;
            writer.flush().map_err(|e| format!("冲刷测试剪贴板失败：{e}"))?;
            drop(writer);
            fs::rename(&temp, &final_path).map_err(|e| format!("发布测试剪贴板失败：{e}"))?;
            Ok(self.publish(token.clone(), final_path.clone(), 0, 0, bytes.len() as u64).token)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
            let _ = fs::remove_file(&final_path);
        }
        result
    }
}

impl Drop for ClipboardStore {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub struct CopyBuilder<'a> {
    store: &'a ClipboardStore,
    text: String,
    bytes: u64,
    file: Option<BufWriter<File>>,
    temp_path: Option<PathBuf>,
    final_path: Option<PathBuf>,
    token: Option<String>,
}

impl<'a> CopyBuilder<'a> {
    pub fn new(store: &'a ClipboardStore) -> Self {
        Self {
            store,
            text: String::new(),
            bytes: 0,
            file: None,
            temp_path: None,
            final_path: None,
            token: None,
        }
    }

    fn spill(&mut self) -> Result<(), String> {
        let required = (self.text.len() as u64).checked_add(COPY_BUFFER_BYTES as u64)
            .ok_or("复制临时空间需求溢出")?;
        let (token, path, mut writer) = self.store.new_temp_file(required)?;
        self.temp_path = Some(path);
        self.token = Some(token);
        let prefix = std::mem::take(&mut self.text);
        writer.write_all(prefix.as_bytes()).map_err(|e| format!("写剪贴板临时文件失败：{e}"))?;
        self.file = Some(writer);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let total = self.bytes.checked_add(bytes.len() as u64).ok_or("复制字节数溢出")?;
        if self.file.is_none() && total <= LARGE_COPY_BYTES {
            let text = std::str::from_utf8(bytes).map_err(|_| "复制文本不是 UTF-8".to_string())?;
            self.text.try_reserve_exact(bytes.len()).map_err(|e| format!("复制内存不足：{e}"))?;
            self.text.push_str(text);
            self.bytes = total;
            return Ok(());
        }
        if self.file.is_none() {
            self.spill()?;
        }
        self.file.as_mut().expect("spill 后必有临时文件")
            .write_all(bytes).map_err(|e| format!("写剪贴板临时文件失败：{e}"))?;
        self.bytes = total;
        Ok(())
    }

    fn write_str(&mut self, value: &str) -> Result<(), String> {
        let mut start = 0usize;
        while start < value.len() {
            let mut end = start.saturating_add(COPY_BUFFER_BYTES).min(value.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.write_bytes(value[start..end].as_bytes())?;
            start = end;
        }
        Ok(())
    }

    fn write_field(&mut self, value: &str) -> Result<(), String> {
        let quoted = value.contains('\t') || value.contains('"')
            || value.contains('\r') || value.contains('\n');
        if quoted {
            self.write_str("\"")?;
        }
        let mut start = 0usize;
        for (index, _) in value.match_indices('"') {
            self.write_str(&value[start..index])?;
            self.write_str("\"\"")?;
            start = index + 1;
        }
        self.write_str(&value[start..])?;
        if quoted {
            self.write_str("\"")?;
        }
        Ok(())
    }

    pub fn write_record(&mut self, fields: &[String], c0: u32, c1: u32) -> Result<(), String> {
        for col in c0..c1 {
            if col > c0 {
                self.write_str("\t")?;
            }
            let value = fields.get(col as usize).map(String::as_str).unwrap_or("");
            self.write_field(value)?;
        }
        self.write_str("\r\n")
    }

    pub fn finish(mut self, rows: u64, cols: u32) -> Result<CopyOutcome, String> {
        if let Some(mut file) = self.file.take() {
            file.flush().map_err(|e| format!("冲刷剪贴板临时文件失败：{e}"))?;
        }
        if let Some(temp) = self.temp_path.as_ref().cloned() {
            let token = self.token.clone().ok_or("剪贴板临时文件缺少 token")?;
            let final_path = self.store.root.join(format!("{token}.tsv"));
            fs::rename(&temp, &final_path).map_err(|e| format!("发布剪贴板临时文件失败：{e}"))?;
            self.temp_path = None;
            self.final_path = Some(final_path.clone());
            let meta = self.store.publish(token, final_path, rows, cols, self.bytes);
            self.final_path = None;
            return Ok(CopyOutcome::App { meta });
        }
        let text = std::mem::take(&mut self.text);
        Ok(CopyOutcome::Text { text, bytes: self.bytes })
    }
}

impl Drop for CopyBuilder<'_> {
    fn drop(&mut self) {
        if let Some(path) = &self.temp_path {
            let _ = fs::remove_file(path);
        }
        if let Some(path) = &self.final_path {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardStore;

    #[test]
    fn replacing_copy_removes_only_previous_owned_file() {
        let store = ClipboardStore::create_unique().unwrap();
        let first = store.put_test_bytes(b"first").unwrap();
        let old = store.get(&first).unwrap().path.clone();
        let second = store.put_test_bytes(b"second").unwrap();
        assert_ne!(first, second);
        assert!(!old.exists());
        assert!(store.get(&second).unwrap().path.exists());
        store.cleanup();
    }
}
