use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

static LIVE_RESERVED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 连续私有区域在并行写入各阶段发出的诊断事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionEvent {
    WorkerBegin { worker: usize },
    WorkerEnd { worker: usize },
    BeforeEnable { offset: usize, len: usize },
    Enabled { offset: usize, len: usize },
}

/// 一段预计算的绝对字节区间，用于并行直写新正文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeSpan {
    pub offset: usize,
    pub len: usize,
}

/// 已预留但尚未完整写入的连续私有内存区域。
pub struct RamRegion {
    base: NonNull<u8>,
    len: usize,
    enabled_bytes: AtomicUsize,
    written_bytes: AtomicUsize,
    started: bool,
    /// 已进入预计算区间并行写入模式；两种写入方式互斥。
    range_mode: bool,
    enable_fault_at: Option<usize>,
}

/// 所有字节均已写入且所有写入线程均已退出的只读完成态。
pub struct FinishedRam(RamRegion);

impl fmt::Debug for FinishedRam {
    /// 只输出长度，避免在日志中展开整份文档。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FinishedRam")
            .field("len", &self.len())
            .finish()
    }
}

// SAFETY: 安全并行入口只发放唯一原子下标对应的不相交范围，所有线程在返回前 joined。
unsafe impl Send for RamRegion {}
// SAFETY: 共享引用只能取得原子状态；可写区间由唯一的下标发放机制隔离。
unsafe impl Sync for RamRegion {}

/// 返回本模块当前持有的预留字节数，仅供隔离诊断使用。
pub fn live_reserved_bytes() -> usize {
    LIVE_RESERVED_BYTES.load(Ordering::Relaxed)
}

#[cfg(test)]
std::thread_local! {
    /// 本线程创建且尚未释放的预留字节数；并发测试之间互不干扰。
    static THREAD_RESERVED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 返回本线程当前持有的预留字节数，仅供内存记账测试使用。
#[cfg(test)]
pub fn thread_reserved_bytes() -> usize {
    THREAD_RESERVED_BYTES.with(|bytes| bytes.get())
}

impl RamRegion {
    /// 预留 `len` 字节的连续地址；失败时不影响调用方现有后备。
    pub fn reserve(len: usize) -> Result<Self, String> {
        Self::reserve_with_enable_fault(len, None)
    }

    /// 预留区域并可选择在指定块偏移注入一次启用失败，供恢复验收使用。
    #[doc(hidden)]
    pub fn reserve_with_enable_fault(
        len: usize,
        enable_fault_at: Option<usize>,
    ) -> Result<Self, String> {
        if len == 0 {
            return Err("不能预留 0 字节内存区域".into());
        }
        let base = os::reserve(len)?;
        LIVE_RESERVED_BYTES.fetch_add(len, Ordering::Relaxed);
        #[cfg(test)]
        THREAD_RESERVED_BYTES.with(|bytes| bytes.set(bytes.get() + len));
        Ok(Self {
            base,
            len,
            enabled_bytes: AtomicUsize::new(0),
            written_bytes: AtomicUsize::new(0),
            started: false,
            range_mode: false,
            enable_fault_at,
        })
    }

    /// 返回连续区域的逻辑字节长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 返回区域基址，供平台提交查询和只读完成态构造使用。
    pub fn as_ptr(&self) -> *const u8 {
        self.base.as_ptr()
    }

    /// 返回已经成功启用的逻辑字节数。
    pub fn enabled_bytes(&self) -> usize {
        self.enabled_bytes.load(Ordering::Relaxed)
    }

    /// 用有界 scoped threads 写入不相交块；错误停止调度并等待在途写入退出。
    pub fn for_each_chunk_mut<F, O>(
        &mut self,
        chunk_bytes: usize,
        workers: usize,
        observer: O,
        writer: F,
    ) -> Result<(), String>
    where
        F: Fn(usize, &mut [u8]) -> Result<(), String> + Sync,
        O: Fn(RegionEvent) + Sync,
    {
        if self.started {
            return Err("内存区域已经开始写入".into());
        }
        if chunk_bytes == 0 || workers == 0 {
            return Err("块大小和并行度必须大于 0".into());
        }
        self.started = true;
        let shared = &*self;
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let error = Mutex::new(None::<String>);
        let count = workers.min(self.len.div_ceil(chunk_bytes)).max(1);
        let record = |text: String| {
            stop.store(true, Ordering::Relaxed);
            let mut slot = error.lock().unwrap();
            if slot.is_none() {
                *slot = Some(text);
            }
        };
        std::thread::scope(|scope| {
            for worker in 0..count {
                let writer = &writer;
                let observer = &observer;
                let record = &record;
                let next = &next;
                let stop = &stop;
                scope.spawn(move || {
                    observer(RegionEvent::WorkerBegin { worker });
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(start) = index.checked_mul(chunk_bytes) else {
                            record("内存块偏移溢出".into());
                            break;
                        };
                        if start >= shared.len {
                            break;
                        }
                        let len = chunk_bytes.min(shared.len - start);
                        observer(RegionEvent::BeforeEnable { offset: start, len });
                        if shared.enable_fault_at == Some(start) {
                            record(format!("注入提交失败，偏移 {start}"));
                            break;
                        }
                        if let Err(text) = os::enable(shared.base, start, len) {
                            record(text);
                            break;
                        }
                        shared.enabled_bytes.fetch_add(len, Ordering::Relaxed);
                        observer(RegionEvent::Enabled { offset: start, len });
                        // SAFETY: start 只发放一次，范围有效且与其他 worker 不相交。
                        let out = unsafe {
                            std::slice::from_raw_parts_mut(shared.base.as_ptr().add(start), len)
                        };
                        if let Err(text) = writer(start, out) {
                            record(text);
                            break;
                        }
                        shared.written_bytes.fetch_add(len, Ordering::Relaxed);
                    }
                    observer(RegionEvent::WorkerEnd { worker });
                });
            }
        });
        if let Some(text) = error.into_inner().unwrap() {
            Err(text)
        } else {
            Ok(())
        }
    }

    /// 用有界 scoped threads 并行写入预计算的不相交区间。
    ///
    /// 区间必须从偏移 0 起精确铺满整个区域：相邻区间无间隙、无重叠，
    /// 最后一个区间恰好结束于区域长度。所有校验都在标记 `started` 之前完成，
    /// 失败时区域仍可走顺序 [`RamRegion::append`]。
    ///
    /// 调用线程在等待工作线程期间反复调用 `poll`，使进度回调无需跨线程。
    ///
    /// # Parameters
    /// - `ranges`: 覆盖整个区域的预计算区间表。
    /// - `workers`: 并行度；大于区间数时按区间数截断。
    /// - `observer`: 区域阶段事件回调，可从多个线程调用。
    /// - `writer`: 接收区间下标与独占切片；仅在返回 `Ok` 时累计写入字节。
    /// - `poll`: 调用线程等待期的轮询钩子。
    ///
    /// # Returns
    /// 所有区间都成功写入时返回 `Ok(())`。
    ///
    /// # Errors
    /// 区间校验失败、平台提交失败、故障注入或任一 writer 失败时返回首个错误。
    pub fn for_each_range_mut<F, O, P>(
        &mut self,
        ranges: &[RangeSpan],
        workers: usize,
        observer: O,
        writer: F,
        mut poll: P,
    ) -> Result<(), String>
    where
        F: Fn(usize, &mut [u8]) -> Result<(), String> + Sync,
        O: Fn(RegionEvent) + Sync,
        P: FnMut(),
    {
        if self.started {
            return Err("内存区域已经开始写入".into());
        }
        if workers == 0 {
            return Err("并行度必须大于 0".into());
        }
        if ranges.is_empty() {
            return Err("范围表不能为空".into());
        }
        let mut cursor = 0usize;
        for range in ranges {
            if range.len == 0 {
                return Err("范围长度必须大于 0".into());
            }
            let end = range
                .offset
                .checked_add(range.len)
                .ok_or_else(|| "范围偏移溢出".to_string())?;
            if range.offset != cursor {
                return Err("范围表必须无间隙、无重叠且按偏移升序".into());
            }
            if end > self.len {
                return Err("范围超出预留区域".into());
            }
            cursor = end;
        }
        if cursor != self.len {
            return Err("范围表必须恰好覆盖整个内存区域".into());
        }
        self.started = true;
        self.range_mode = true;
        let shared = &*self;
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let error = Mutex::new(None::<String>);
        let finished = AtomicUsize::new(0);
        let count = workers.min(ranges.len()).max(1);
        let record = |text: String| {
            stop.store(true, Ordering::Relaxed);
            let mut slot = error.lock().unwrap();
            if slot.is_none() {
                *slot = Some(text);
            }
        };
        std::thread::scope(|scope| {
            for worker in 0..count {
                let writer = &writer;
                let observer = &observer;
                let record = &record;
                let next = &next;
                let stop = &stop;
                let finished = &finished;
                let spawned = std::thread::Builder::new()
                    // 物化期间线程栈计入私有内存，64 MiB 门禁余量有限。
                    .stack_size(512 * 1024)
                    .spawn_scoped(scope, move || {
                        observer(RegionEvent::WorkerBegin { worker });
                        loop {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            if index >= ranges.len() {
                                break;
                            }
                            let range = ranges[index];
                            observer(RegionEvent::BeforeEnable {
                                offset: range.offset,
                                len: range.len,
                            });
                            if let Some(fault) = shared.enable_fault_at {
                                // 故障偏移落在区间内即注入，覆盖任意变长区间。
                                if fault >= range.offset
                                    && fault < range.offset + range.len
                                {
                                    record(format!("注入提交失败，偏移 {fault}"));
                                    break;
                                }
                            }
                            if let Err(text) =
                                os::enable(shared.base, range.offset, range.len)
                            {
                                record(text);
                                break;
                            }
                            shared
                                .enabled_bytes
                                .fetch_add(range.len, Ordering::Relaxed);
                            observer(RegionEvent::Enabled {
                                offset: range.offset,
                                len: range.len,
                            });
                            // SAFETY: index 只发放一次，区间已校验互不相交且在预留区域内。
                            let out = unsafe {
                                std::slice::from_raw_parts_mut(
                                    shared.base.as_ptr().add(range.offset),
                                    range.len,
                                )
                            };
                            if let Err(text) = writer(index, out) {
                                record(text);
                                break;
                            }
                            shared.written_bytes.fetch_add(range.len, Ordering::Relaxed);
                            finished.fetch_add(1, Ordering::Relaxed);
                        }
                        observer(RegionEvent::WorkerEnd { worker });
                    });
                if spawned.is_err() {
                    record("创建并行写入线程失败".into());
                }
            }
            // 调用线程等待期间轮询进度；回调不会跨线程。
            while finished.load(Ordering::Relaxed) < ranges.len()
                && error.lock().unwrap().is_none()
            {
                poll();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        if let Some(text) = error.into_inner().unwrap() {
            Err(text)
        } else {
            Ok(())
        }
    }

    /// 只有完整写入才产生只读完成态；失败会消耗区域并由 Drop 释放。
    pub fn finish(self) -> Result<FinishedRam, String> {
        if self.written_bytes.load(Ordering::Relaxed) != self.len {
            return Err("内存区域尚未完整写入".into());
        }
        Ok(FinishedRam(self))
    }
}

impl FinishedRam {
    /// 暴露连续只读字节；完成态保证所有写入线程退出且写入量等于区域长度。
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: 完成态保证整个区域已初始化，且此后没有安全写入入口。
        unsafe { std::slice::from_raw_parts(self.0.base.as_ptr(), self.0.len) }
    }

    /// 返回完成态的逻辑字节长度。
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// 返回完成态基址，供匿名区域审计使用。
    pub fn as_ptr(&self) -> *const u8 {
        self.0.base.as_ptr()
    }
}

impl RamRegion {
    /// 顺序追加字节，按需以 64 KiB 为粒度提交目标私有页。
    ///
    /// # Parameters
    /// - `bytes`: 待复制到当前写入末尾的连续字节。
    ///
    /// # Returns
    /// 成功时完整复制输入；失败时不保证本次输入已部分复制，但既有写入保持有效。
    ///
    /// # Errors
    /// 输入越过预留长度、偏移溢出或平台提交失败时返回错误。
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        const COMMIT_BYTES: usize = 64 * 1024;
        if self.range_mode {
            return Err("内存区域已经开始写入".into());
        }
        let initial = self.written_bytes.load(Ordering::Relaxed);
        let end = initial
            .checked_add(bytes.len())
            .ok_or_else(|| "顺序写入长度溢出".to_string())?;
        if end > self.len {
            return Err("顺序写入超出预留区域".into());
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.started = true;
        let mut written = initial;
        let mut input_offset = 0usize;
        while written < end {
            let mut enabled = self.enabled_bytes.load(Ordering::Relaxed);
            if written == enabled {
                let commit_len = COMMIT_BYTES.min(self.len - written);
                if self.enable_fault_at == Some(written) {
                    return Err(format!("注入提交失败，偏移 {written}"));
                }
                os::enable(self.base, written, commit_len)?;
                enabled = written + commit_len;
                self.enabled_bytes.store(enabled, Ordering::Relaxed);
            }
            if written > enabled {
                return Err("顺序写入提交水位异常".into());
            }
            let count = (end - written).min(enabled - written);
            // SAFETY: &mut self 保证唯一写入，目标区间已提交且不越过预留长度。
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(input_offset),
                    self.base.as_ptr().add(written),
                    count,
                );
            }
            written += count;
            input_offset += count;
            self.written_bytes.store(written, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl Drop for RamRegion {
    /// 释放整段平台区域并扣减隔离诊断计数。
    fn drop(&mut self) {
        os::free(self.base, self.len);
        LIVE_RESERVED_BYTES.fetch_sub(self.len, Ordering::Relaxed);
        #[cfg(test)]
        THREAD_RESERVED_BYTES.with(|bytes| bytes.set(bytes.get().saturating_sub(self.len)));
    }
}

#[cfg(windows)]
mod os {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
    };

    /// 只预留连续地址，不提前提交整个文件大小的私有内存。
    pub fn reserve(len: usize) -> Result<NonNull<u8>, String> {
        // SAFETY: 返回的新区域由 RamRegion 独占，并在 Drop 中整段释放。
        let ptr = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE, PAGE_READWRITE) };
        NonNull::new(ptr.cast())
            .ok_or_else(|| format!("预留 {len} 字节失败：{}", std::io::Error::last_os_error()))
    }

    /// 提交一个已经预留且与其他 worker 不相交的目标块。
    pub fn enable(base: NonNull<u8>, start: usize, len: usize) -> Result<(), String> {
        start
            .checked_add(len)
            .ok_or_else(|| "提交块范围溢出".to_string())?;
        // SAFETY: 调用方保证 start..start+len 位于预留区域内且只提交一次。
        let ptr = unsafe {
            VirtualAlloc(
                base.as_ptr().add(start).cast(),
                len,
                MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if ptr.is_null() {
            Err(format!(
                "提交偏移 {start} 的 {len} 字节失败：{}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    /// 释放与成功预留完全对应的整段基址。
    pub fn free(base: NonNull<u8>, _len: usize) {
        // SAFETY: base 来自成功的 VirtualAlloc，MEM_RELEASE 要求大小为 0。
        let result = unsafe { VirtualFree(base.as_ptr().cast(), 0, MEM_RELEASE) };
        debug_assert_ne!(
            result,
            0,
            "VirtualFree 失败：{}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;

    /// 创建匿名私有映射，物理页在后续写入时按需分配。
    pub fn reserve(len: usize) -> Result<NonNull<u8>, String> {
        // SAFETY: 返回新的匿名私有映射，成功时由 RamRegion::drop 释放。
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            Err(format!(
                "匿名映射 {len} 字节失败：{}",
                std::io::Error::last_os_error()
            ))
        } else {
            NonNull::new(ptr.cast()).ok_or_else(|| "匿名映射返回空地址".into())
        }
    }

    /// 校验块范围；macOS 不伪造 Windows 式逐块 commit。
    pub fn enable(_base: NonNull<u8>, start: usize, len: usize) -> Result<(), String> {
        start
            .checked_add(len)
            .ok_or_else(|| "匿名块范围溢出".to_string())?;
        Ok(())
    }

    /// 解除与成功匿名映射完全对应的整段区域。
    pub fn free(base: NonNull<u8>, len: usize) {
        // SAFETY: base/len 与成功 mmap 完全一致，Drop 只执行一次。
        let result = unsafe { libc::munmap(base.as_ptr().cast(), len) };
        debug_assert_eq!(
            result,
            0,
            "munmap 失败：{}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{FinishedRam, RamRegion, RangeSpan, RegionEvent};

    #[test]
    fn parallel_chunks_cover_every_byte_and_finish() {
        let len = 4 * 1024 * 1024 + 123;
        let mut region = RamRegion::reserve(len).unwrap();
        region
            .for_each_chunk_mut(
                64 * 1024,
                4,
                |_event: RegionEvent| {},
                |start, out| {
                    for (i, byte) in out.iter_mut().enumerate() {
                        *byte = ((start + i) % 251) as u8;
                    }
                    Ok(())
                },
            )
            .unwrap();
        let finished: FinishedRam = region.finish().unwrap();
        assert_eq!(finished.len(), len);
        assert!(finished
            .as_slice()
            .iter()
            .enumerate()
            .all(|(i, b)| *b == (i % 251) as u8));
    }

    #[test]
    fn failed_chunk_cannot_finish() {
        let mut region = RamRegion::reserve(1024 * 1024).unwrap();
        let error = region
            .for_each_chunk_mut(
                64 * 1024,
                2,
                |_| {},
                |start, _| {
                    if start == 128 * 1024 {
                        Err("injected write".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(error.contains("injected write"));
        assert!(region.finish().unwrap_err().contains("尚未完整写入"));
    }

    #[test]
    fn range_writer_fills_exact_ranges_and_finish_checks_total() {
        let len = 13usize;
        let ranges = [
            RangeSpan { offset: 0, len: 5 },
            RangeSpan { offset: 5, len: 7 },
            RangeSpan { offset: 12, len: 1 },
        ];
        let mut expected = vec![0u8; len];
        for (index, range) in ranges.iter().enumerate() {
            for byte in &mut expected[range.offset..range.offset + range.len] {
                *byte = (index * 10) as u8;
            }
        }
        let events = std::sync::Mutex::new(Vec::new());
        let mut region = RamRegion::reserve(len).unwrap();
        region
            .for_each_range_mut(
                &ranges,
                3,
                |event| events.lock().unwrap().push(event),
                |index, out| {
                    out.fill((index * 10) as u8);
                    Ok(())
                },
                || {},
            )
            .unwrap();
        assert_eq!(region.enabled_bytes(), len);
        let finished: FinishedRam = region.finish().unwrap();
        assert_eq!(finished.as_slice(), expected.as_slice());
        let events = events.into_inner().unwrap();
        let begins = events
            .iter()
            .filter(|event| matches!(event, RegionEvent::WorkerBegin { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|event| matches!(event, RegionEvent::WorkerEnd { .. }))
            .count();
        assert_eq!(begins, 3);
        assert_eq!(ends, 3);
        let enabled = events
            .iter()
            .filter(|event| matches!(event, RegionEvent::Enabled { .. }))
            .count();
        assert_eq!(enabled, ranges.len());
    }

    #[test]
    fn range_writer_rejects_overlap_and_out_of_bounds() {
        let len = 13usize;
        let invalid: Vec<Vec<RangeSpan>> = vec![
            vec![],
            vec![RangeSpan { offset: 0, len: 0 }],
            vec![RangeSpan { offset: 1, len: 12 }],
            vec![
                RangeSpan { offset: 0, len: 5 },
                RangeSpan { offset: 6, len: 7 },
            ],
            vec![
                RangeSpan { offset: 0, len: 5 },
                RangeSpan { offset: 4, len: 9 },
            ],
            vec![RangeSpan { offset: 0, len: 12 }],
            vec![
                RangeSpan { offset: 0, len: 13 },
                RangeSpan {
                    offset: 13,
                    len: 1,
                },
            ],
            vec![RangeSpan {
                offset: usize::MAX - 1,
                len: 4,
            }],
        ];
        for ranges in invalid {
            let mut region = RamRegion::reserve(len).unwrap();
            let error = region
                .for_each_range_mut(&ranges, 1, |_| {}, |_, _| Ok(()), || {})
                .unwrap_err();
            assert!(
                error.contains("范围") || error.contains("覆盖") || error.contains("溢出"),
                "{ranges:?} -> {error}",
            );
            // 校验失败不得把区域标记为已开始写入。
            region.append(&[0u8; 13]).unwrap();
        }
    }

    #[test]
    fn range_writer_injects_enable_fault_inside_range() {
        let ranges = [
            RangeSpan { offset: 0, len: 8 },
            RangeSpan { offset: 8, len: 8 },
        ];
        let mut region = RamRegion::reserve_with_enable_fault(16, Some(8)).unwrap();
        let error = region
            .for_each_range_mut(&ranges, 2, |_| {}, |_, out| {
                out.fill(1);
                Ok(())
            }, || {})
            .unwrap_err();
        assert!(error.contains("注入提交失败，偏移 8"), "{error}");
        assert!(region.finish().unwrap_err().contains("尚未完整写入"));
    }

    #[test]
    fn range_writer_credits_only_successful_ranges() {
        let ranges = [
            RangeSpan { offset: 0, len: 8 },
            RangeSpan { offset: 8, len: 8 },
        ];
        let mut region = RamRegion::reserve(16).unwrap();
        let error = region
            .for_each_range_mut(
                &ranges,
                1,
                |_| {},
                |index, out| {
                    if index == 1 {
                        out[..4].fill(9);
                        return Err("injected range".into());
                    }
                    out.fill(1);
                    Ok(())
                },
                || {},
            )
            .unwrap_err();
        assert!(error.contains("injected range"), "{error}");
        assert_eq!(region.enabled_bytes(), 16);
        assert!(region.finish().unwrap_err().contains("尚未完整写入"));
    }

    #[test]
    fn range_writer_is_exclusive_with_append_and_chunk_api() {
        let ranges = [RangeSpan {
            offset: 0,
            len: 16,
        }];
        let mut region = RamRegion::reserve(16).unwrap();
        region
            .for_each_range_mut(&ranges, 1, |_| {}, |_, out| {
                out.fill(3);
                Ok(())
            }, || {})
            .unwrap();
        assert!(region.append(&[0]).unwrap_err().contains("已经开始写入"));
        let error = region
            .for_each_chunk_mut(8, 1, |_| {}, |_, out| {
                out.fill(4);
                Ok(())
            })
            .unwrap_err();
        assert!(error.contains("已经开始写入"), "{error}");
    }

    #[test]
    fn range_writer_handles_unaligned_boundaries() {
        for _ in 0..20 {
            let ranges = [
                RangeSpan {
                    offset: 0,
                    len: 1000,
                },
                RangeSpan {
                    offset: 1000,
                    len: 1000,
                },
                RangeSpan {
                    offset: 2000,
                    len: 1000,
                },
            ];
            let mut region = RamRegion::reserve(3000).unwrap();
            region
                .for_each_range_mut(&ranges, 3, |_| {}, |index, out| {
                    out.fill(index as u8);
                    Ok(())
                }, || {})
                .unwrap();
            let finished = region.finish().unwrap();
            assert_eq!(finished.as_slice().len(), 3000);
            assert!(finished.as_slice()[1500] == 1);
            assert!(finished.as_slice()[2999] == 2);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_reserve_is_not_committed_and_chunks_commit_monotonically() {
        use windows_sys::Win32::System::Memory::{
            VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_FREE,
        };
        fn committed(mut address: usize, end: usize) -> usize {
            let mut total = 0;
            while address < end {
                let mut info = unsafe { std::mem::zeroed() };
                let size = unsafe {
                    VirtualQuery(
                        address as _,
                        &mut info,
                        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                    )
                };
                assert_ne!(size, 0);
                if info.State == MEM_COMMIT {
                    total += info.RegionSize;
                }
                address += info.RegionSize;
            }
            total
        }
        let len = 2 * 1024 * 1024 + 17;
        let mut region = RamRegion::reserve(len).unwrap();
        let base = region.as_ptr() as usize;
        assert_eq!(committed(base, base + len), 0);
        region
            .for_each_chunk_mut(
                64 * 1024,
                1,
                |_| {},
                |_, out| {
                    out.fill(7);
                    Ok(())
                },
            )
            .unwrap();
        let finished = region.finish().unwrap();
        assert!(committed(base, base + len) >= len);
        let base = finished.as_ptr();
        drop(finished);
        let mut info = unsafe { std::mem::zeroed() };
        unsafe {
            VirtualQuery(
                base as _,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
        }
        assert_eq!(info.State, MEM_FREE);
    }
}
