use crate::ram_region::{FinishedRam, RamRegion, RegionEvent};
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const CONVERSION_MARGIN_PERMILLE: u64 = 50;
pub const COPY_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_LOAD_WORKERS: usize = 4;

/// 计算 floor(X × 1.05)，任一乘法或加法溢出都拒绝转换。
pub fn conversion_limit_bytes(file_bytes: u64) -> Result<u64, String> {
    file_bytes
        .checked_mul(1000 + CONVERSION_MARGIN_PERMILLE)
        .and_then(|value| value.checked_div(1000))
        .ok_or_else(|| "转换内存上限计算溢出".into())
}

/// 受控故障只用于诊断恢复路径，正常载入使用 None。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadFault {
    #[default]
    None,
    CommitAt(usize),
    ReadAfter(u64),
}

/// 分块载入参数；默认最多使用四个 worker 和 8 MiB 块。
#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    pub chunk_bytes: usize,
    pub workers: usize,
    #[doc(hidden)]
    pub fault: LoadFault,
}

impl Default for LoadOptions {
    fn default() -> Self {
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_LOAD_WORKERS);
        Self {
            chunk_bytes: COPY_CHUNK_BYTES,
            workers,
            fault: LoadFault::None,
        }
    }
}

/// 平台中性的载入阶段事件，供进度显示和真实内存采样使用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadEvent {
    ReserveBegin { total: u64 },
    Reserved { total: u64 },
    WorkerBegin { worker: usize },
    WorkerEnd { worker: usize },
    ChunkBegin { offset: u64, len: usize },
    ChunkReady { offset: u64, len: usize },
    Progress { done: u64, total: u64 },
    Finished { total: u64 },
}

/// 只有完整完成或取消两种成功结果，失败始终保留错误。
#[derive(Debug)]
pub enum LoadResult {
    Complete(FinishedRam),
    Cancelled,
}

/// 线程安全定位读取；实现不得改变共享文件游标。
pub trait PositionedRead: Sync {
    fn len(&self) -> Result<u64, String>;
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String>;
}

#[cfg(windows)]
impl PositionedRead for File {
    fn len(&self) -> Result<u64, String> {
        Ok(self.metadata().map_err(|error| error.to_string())?.len())
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        use std::os::windows::fs::FileExt;
        self.seek_read(out, offset)
            .map_err(|error| format!("定位读取失败，偏移 {offset}：{error}"))
    }
}

#[cfg(unix)]
impl PositionedRead for File {
    fn len(&self) -> Result<u64, String> {
        Ok(self.metadata().map_err(|error| error.to_string())?.len())
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        use std::os::unix::fs::FileExt;
        FileExt::read_at(self, out, offset)
            .map_err(|error| format!("定位读取失败，偏移 {offset}：{error}"))
    }
}

/// 整块复制数据源；成功返回时 out 必须全部填满。
pub trait ChunkCopy: Sync {
    /// 返回源长度，失败时保留平台错误。
    fn len(&self) -> Result<u64, String>;
    /// 把 offset 对应的完整目标块复制到 out，失败时返回中文错误。
    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String>;
}

impl<T: PositionedRead> ChunkCopy for T {
    fn len(&self) -> Result<u64, String> {
        PositionedRead::len(self)
    }

    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let mut read = 0usize;
        while read < out.len() {
            let current = offset.checked_add(read as u64).ok_or("读取偏移溢出")?;
            let count = self.read_at(current, &mut out[read..])?;
            if count == 0 {
                return Err(format!("文件读取提前结束，偏移 {current}"));
            }
            read += count;
        }
        Ok(())
    }
}

/// 借用首次转换前的完整 M0；调用方保证整个复制期间原映射保持有效。
pub(crate) struct MmapSource<'a> {
    mmap: &'a memmap2::Mmap,
}

impl<'a> MmapSource<'a> {
    pub(crate) fn new(mmap: &'a memmap2::Mmap) -> Self {
        Self { mmap }
    }
}

impl ChunkCopy for MmapSource<'_> {
    fn len(&self) -> Result<u64, String> {
        Ok(self.mmap.len() as u64)
    }

    fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|_| "M0 复制偏移溢出")?;
        let end = start.checked_add(out.len()).ok_or("M0 复制长度溢出")?;
        let source = self
            .mmap
            .get(start..end)
            .ok_or_else(|| format!("M0 复制范围超出源长度：{start}..{end}"))?;
        out.copy_from_slice(source);
        Ok(())
    }
}

/// 已预留目标区域但仍持有来源的待执行载入。
pub struct PreparedLoad<R> {
    source: R,
    region: RamRegion,
    total: usize,
    options: LoadOptions,
}

/// 第一阶段只取得长度并预留目标；源 mmap 由调用方继续保持。
pub fn prepare_load<R, O>(
    source: R,
    options: LoadOptions,
    observer: &O,
) -> Result<PreparedLoad<R>, String>
where
    R: ChunkCopy,
    O: Fn(LoadEvent) + Sync,
{
    let total =
        usize::try_from(source.len()?).map_err(|_| "文件大小超出本机地址范围".to_string())?;
    observer(LoadEvent::ReserveBegin {
        total: total as u64,
    });
    let fault = match options.fault {
        LoadFault::CommitAt(at) => Some(at),
        _ => None,
    };
    let region = RamRegion::reserve_with_enable_fault(total, fault)?;
    observer(LoadEvent::Reserved {
        total: total as u64,
    });
    observer(LoadEvent::Progress {
        done: 0,
        total: total as u64,
    });
    Ok(PreparedLoad {
        source,
        region,
        total,
        options,
    })
}

/// 第二阶段从 PreparedLoad 持有的来源直接复制到最终区域。
/// 对 MmapSource，原 M0 由调用方保留到本函数返回并完成外部状态检查之后。
pub fn run_prepared_load<R, O>(
    prepared: PreparedLoad<R>,
    cancel: &AtomicBool,
    observer: O,
) -> Result<LoadResult, String>
where
    R: ChunkCopy,
    O: Fn(LoadEvent) + Sync,
{
    let PreparedLoad {
        source,
        mut region,
        total,
        options,
    } = prepared;
    if cancel.load(Ordering::Relaxed) {
        return Ok(LoadResult::Cancelled);
    }
    let progress = Mutex::new(0u64);
    let observer = &observer;
    let result = region.for_each_chunk_mut(
        options.chunk_bytes,
        options.workers,
        |event| {
            observer(match event {
                RegionEvent::WorkerBegin { worker } => LoadEvent::WorkerBegin { worker },
                RegionEvent::WorkerEnd { worker } => LoadEvent::WorkerEnd { worker },
                RegionEvent::BeforeEnable { offset, len } => LoadEvent::ChunkBegin {
                    offset: offset as u64,
                    len,
                },
                RegionEvent::Enabled { offset, len } => LoadEvent::ChunkReady {
                    offset: offset as u64,
                    len,
                },
            })
        },
        |start, out| {
            if cancel.load(Ordering::Relaxed) {
                return Err("用户取消载入".into());
            }
            let offset = start as u64;
            if matches!(options.fault, LoadFault::ReadAfter(limit) if offset >= limit) {
                return Err(format!("注入读取失败，偏移 {offset}"));
            }
            source.copy_chunk(offset, out)?;
            // 先确定取消结果再发布进度；最终进度发出后不再因同一标志回滚成取消。
            if cancel.load(Ordering::Relaxed) {
                return Err("用户取消载入".into());
            }
            let mut done = progress.lock().unwrap();
            *done = done.checked_add(out.len() as u64).ok_or("读取进度溢出")?;
            observer(LoadEvent::Progress {
                done: *done,
                total: total as u64,
            });
            Ok(())
        },
    );
    if let Err(error) = result {
        if cancel.load(Ordering::Relaxed) {
            return Ok(LoadResult::Cancelled);
        }
        return Err(error);
    }
    let finished = region.finish()?;
    observer(LoadEvent::Finished {
        total: total as u64,
    });
    Ok(LoadResult::Complete(finished))
}

/// 小测试和不需要解除源映射的调用方使用两阶段核心，不另写复制算法。
pub fn load_file<R, O>(
    source: R,
    cancel: &AtomicBool,
    options: LoadOptions,
    observer: O,
) -> Result<LoadResult, String>
where
    R: ChunkCopy,
    O: Fn(LoadEvent) + Sync,
{
    let prepared = prepare_load(source, options, &observer)?;
    run_prepared_load(prepared, cancel, observer)
}

#[cfg(test)]
mod tests {
    use super::{
        conversion_limit_bytes, load_file, ChunkCopy, LoadEvent, LoadFault, LoadOptions,
        LoadResult, MmapSource,
    };
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);

    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// 生成唯一合成文件，返回读取句柄和自动清理守卫。
    fn temp_pattern_file(name: &str, len: usize) -> (File, Cleanup) {
        let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "simpletable_ram_load_{name}_{}_{id}.bin",
            std::process::id()
        ));
        let mut file = File::create(&path).unwrap();
        let mut buffer = vec![0u8; (1024 * 1024).min(len.max(1))];
        let mut offset = 0usize;
        while offset < len {
            let count = buffer.len().min(len - offset);
            for (index, byte) in buffer[..count].iter_mut().enumerate() {
                *byte = ((offset + index) % 251) as u8;
            }
            file.write_all(&buffer[..count]).unwrap();
            offset += count;
        }
        file.sync_all().unwrap();
        drop(file);
        (File::open(&path).unwrap(), Cleanup(path))
    }

    #[test]
    fn conversion_limit_is_exactly_five_percent_and_checked() {
        assert_eq!(conversion_limit_bytes(0).unwrap(), 0);
        assert_eq!(conversion_limit_bytes(100).unwrap(), 105);
        assert_eq!(conversion_limit_bytes(101).unwrap(), 106);
        assert_eq!(
            conversion_limit_bytes(20 << 30).unwrap(),
            (20 << 30) + (1 << 30)
        );
        assert!(conversion_limit_bytes(u64::MAX).is_err());
    }

    #[test]
    fn mmap_source_copies_single_multi_and_tail_chunks() {
        let (file, _cleanup) = temp_pattern_file("mmap_source_ranges", 70_000);
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let source = MmapSource::new(&mmap);
        assert_eq!(source.len().unwrap(), 70_000);

        for (start, count) in [(0usize, 1000usize), (999, 1001), (69_999, 1)] {
            let mut out = vec![0u8; count];
            source.copy_chunk(start as u64, &mut out).unwrap();
            assert!(out.iter().enumerate().all(|(index, byte)| {
                *byte == ((start + index) % 251) as u8
            }));
        }
    }

    #[test]
    fn mmap_source_parallel_copy_matches_original_bytes() {
        let (file, _cleanup) = temp_pattern_file("mmap_source_parallel", 3 * 1024 * 1024 + 17);
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let cancel = AtomicBool::new(false);
        let result = load_file(
            MmapSource::new(&mmap),
            &cancel,
            LoadOptions {
                chunk_bytes: 64 * 1024,
                workers: 4,
                fault: LoadFault::None,
            },
            |_| {},
        )
        .unwrap();
        let LoadResult::Complete(ram) = result else {
            panic!("unexpected cancel")
        };
        assert_eq!(ram.as_slice(), mmap.as_ref());
    }

    #[test]
    fn mmap_source_rejects_out_of_bounds_copy() {
        let (file, _cleanup) = temp_pattern_file("mmap_source_bounds", 257);
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let source = MmapSource::new(&mmap);
        let mut out = [0u8; 2];
        let error = source.copy_chunk(256, &mut out).unwrap_err();
        assert!(error.contains("超出"), "{error}");
        assert_eq!(out, [0, 0], "越界时不得写入部分目标块");
    }

    #[test]
    fn load_reads_real_file_with_bounded_parallelism_and_monotonic_progress() {
        let (file, _cleanup) = temp_pattern_file("ram_load_ok", 3 * 1024 * 1024 + 17);
        let cancel = AtomicBool::new(false);
        let events = Mutex::new(Vec::new());
        let result = load_file(
            file,
            &cancel,
            LoadOptions {
                chunk_bytes: 64 * 1024,
                workers: 4,
                fault: LoadFault::None,
            },
            |event| events.lock().unwrap().push(event),
        )
        .unwrap();
        let LoadResult::Complete(ram) = result else {
            panic!("unexpected cancel")
        };
        assert!(ram
            .as_slice()
            .iter()
            .enumerate()
            .all(|(i, b)| *b == (i % 251) as u8));
        let events = events.into_inner().unwrap();
        let progress: Vec<u64> = events
            .iter()
            .filter_map(|event| match event {
                LoadEvent::Progress { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        assert!(progress.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(progress.last().copied(), Some(3 * 1024 * 1024 + 17));
        assert!(matches!(events.last(), Some(LoadEvent::Finished { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, LoadEvent::ChunkBegin { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, LoadEvent::ChunkReady { .. })));
    }

    #[test]
    fn cancel_drops_partial_region_without_finished_or_fake_hundred_percent() {
        let (file, _cleanup) = temp_pattern_file("ram_load_cancel", 8 * 1024 * 1024);
        let cancel = AtomicBool::new(false);
        let events = Mutex::new(Vec::new());
        let result = load_file(
            file,
            &cancel,
            LoadOptions {
                chunk_bytes: 64 * 1024,
                workers: 2,
                fault: LoadFault::None,
            },
            |event| {
                events.lock().unwrap().push(event);
                if matches!(event, LoadEvent::Progress { done, .. } if done > 0) {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(matches!(result, LoadResult::Cancelled));
        let events = events.into_inner().unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event, LoadEvent::Finished { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, LoadEvent::Progress { done, total } if done == total)));
    }

    #[test]
    fn cancel_after_final_copy_does_not_emit_fake_hundred_percent() {
        struct CancelAfterCopy<'a> {
            file: File,
            cancel: &'a AtomicBool,
        }

        impl ChunkCopy for CancelAfterCopy<'_> {
            fn len(&self) -> Result<u64, String> {
                Ok(self
                    .file
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .len())
            }

            fn copy_chunk(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
                <File as ChunkCopy>::copy_chunk(&self.file, offset, out)?;
                // 模拟取消在最后复制期间生效，早于任何最终进度事件。
                self.cancel.store(true, Ordering::Relaxed);
                Ok(())
            }
        }

        let (file, _cleanup) = temp_pattern_file("ram_load_final_cancel", 64 * 1024);
        let cancel = AtomicBool::new(false);
        let events = Mutex::new(Vec::new());
        let result = load_file(
            CancelAfterCopy {
                file,
                cancel: &cancel,
            },
            &cancel,
            LoadOptions {
                chunk_bytes: 64 * 1024,
                workers: 1,
                fault: LoadFault::None,
            },
            |event| events.lock().unwrap().push(event),
        )
        .unwrap();
        assert!(matches!(result, LoadResult::Cancelled));
        let events = events.into_inner().unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, LoadEvent::Progress { done, total } if done == total)),
            "取消结果不得先发送 100%：{events:?}"
        );
    }

    #[test]
    fn commit_and_short_read_faults_are_real_errors() {
        for (name, fault, expected) in [
            ("commit", LoadFault::CommitAt(128 * 1024), "提交"),
            ("short", LoadFault::ReadAfter(4096), "读取"),
        ] {
            let (file, _cleanup) = temp_pattern_file(name, 1024 * 1024);
            let cancel = AtomicBool::new(false);
            let error = load_file(
                file,
                &cancel,
                LoadOptions {
                    chunk_bytes: 64 * 1024,
                    workers: 2,
                    fault,
                },
                |_| {},
            )
            .unwrap_err();
            assert!(error.contains(expected), "{name}: {error}");
        }
    }

}
