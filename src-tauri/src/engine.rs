//! 大文件数据引擎：mmap + 记录感知索引 + 覆盖层/插入块/删除位图。
//!
//! 设计要点：
//! - 索引期不解析字段内容：memchr 找 '\n'，引号状态机跨行传播（宽松方言——
//!   引号在字段任意位置都可切换，"" 转义），记录起始 = 上一行结束不在引号内
//!   且本行非空白。引号内嵌换行（本产品核心场景）天然支持。
//! - 并行三趟：① 按字节分块 memchr 收行偏移；② 按行号分 band 算引号状态
//!   迁移表（false/true 两种初态各跑一遍稀疏引号扫描）；③ 前缀复合出各 band
//!   初态后逐行产出记录起始表。band 按行对齐，每行恰属一个 band。
//! - 虚拟行空间 vrow ∈ [0, N)：Segment 表把 vrow 映射到 源记录/插入块；
//!   删除 = 位图 + 段分裂；编辑按稳定身份（记录号 / 块号+行+列）进覆盖层，
//!   行结构变化后依然有效。
//! - 撤销 = 逆操作日志（全表快照在大文件上不可行）。

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use memchr::{memchr, memchr_iter};
use rayon::prelude::*;
use std::time::{Duration, Instant};

// ============================================================
// 长操作友好度：低优先级守卫 + 进度节流
// ============================================================

/// 进度回调最小发送间隔：流式重写期间按 80ms 节拍节流（调用点每 16 行
/// 过闸一次，开销可忽略），~12 次/秒肉眼看仍是连续走动，IPC 压力极低；
/// 磁盘 IO 被拖慢（写缓存饱和/杀软扫描）时节拍不变、行号照走，进度条
/// 不会长时间定住——旧 65536 行粒度在龟速段可能十分钟不刷新，看着像卡死。
const PROGRESS_MIN_GAP: Duration = Duration::from_millis(80);

/// 距上次发送是否已满最小间隔（首次调用恒放行）。
///
/// # Parameters
/// - `last`: 上次发送时刻（None = 从未发送，恒放行并记录）。
/// - `gap`: 最小间隔。
///
/// # Returns
/// 本次是否应发送进度。
fn progress_gate(last: &mut Option<Instant>, gap: Duration) -> bool {
    let now = Instant::now();
    match last {
        Some(t) if now.saturating_duration_since(*t) < gap => false,
        _ => {
            *last = Some(now);
            true
        }
    }
}

// ---- 进度条分阶段配平 ----
// 总体进度按千分比切给三个阶段：流式重写 0-700、冲刷落盘/换名 700-780、
// 重建行索引 780-1000（索引本身秒级完成，进度条在阶段文字+秒数上体现，
// 不再单独报点）。加权的目的是任何阶段都不出现"进度条长时间定住"：
// 慢在重写时条在走，慢在落盘时条跳到 70%+ 并显示阶段文字，慢在索引时
// 条停在 78% 但阶段文字与已用秒数持续刷新。

/// 进度阶段：流式重写。
pub const PHASE_STREAM: u8 = 0;
/// 进度阶段：冲刷落盘与换名替换原文件。
pub const PHASE_FLUSH: u8 = 1;
/// 进度阶段：重建行索引（收尾，事件本身即"索引完成"）。
pub const PHASE_REINDEX: u8 = 2;

/// 流式阶段的加权进度：done 行 → 总体 0..70%。
fn stream_weighted(done: u64) -> u64 {
    done * 7 / 10
}

/// 冲刷完成点（总体 72%）。
fn flushed_weighted(total: u64) -> u64 {
    total * 72 / 100
}

/// 换名完成点（总体 78%，之后进入重建索引阶段）。
fn renamed_weighted(total: u64) -> u64 {
    total * 78 / 100
}

/// 整表重写（保存/执行列操作）期间的低优先级守卫：把**当前线程**降到
/// BELOW_NORMAL，让出 UI 渲染/系统服务的调度优先级；进程内其它线程
/// （界面消息泵、进度转发线程）保持原优先级。曾用进程级
/// PROCESS_MODE_BACKGROUND_BEGIN 实测重写耗时翻倍——内存优先级随进程
/// 整体下调，流式写盘被拖慢；线程级只降调度优先级，无该副作用。
/// 离开作用域时恢复进入前的优先级。
#[cfg(windows)]
mod bg {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;

    extern "system" {
        fn GetCurrentThread() -> Handle;
        fn SetThreadPriority(h: Handle, priority: i32) -> i32;
        fn GetThreadPriority(h: Handle) -> i32;
    }

    /// 进入即降低当前线程优先级，Drop 时恢复到进入前的值。
    pub struct BackgroundGuard {
        handle: Handle,
        prev: i32,
    }

    impl BackgroundGuard {
        /// 降低当前线程优先级；返回持有即保持、析构即恢复的守卫。
        pub fn enter() -> Self {
            unsafe {
                let handle = GetCurrentThread();
                let prev = GetThreadPriority(handle);
                SetThreadPriority(handle, THREAD_PRIORITY_BELOW_NORMAL);
                BackgroundGuard { handle, prev }
            }
        }
    }

    impl Drop for BackgroundGuard {
        fn drop(&mut self) {
            unsafe {
                SetThreadPriority(self.handle, self.prev);
            }
        }
    }
}

/// 非 Windows 平台：守卫为空结构（Tauri 前端与引擎逻辑跨平台一致，
/// 优先级隔离仅 Windows 需要——macOS 的 QoS 由 GCD 自动处理）。
#[cfg(not(windows))]
mod bg {
    pub struct BackgroundGuard;

    impl BackgroundGuard {
        pub fn enter() -> Self {
            BackgroundGuard
        }
    }
}

// ============================================================
// CSV 方言（frontend/js/csv.js parseCsv 的逐字节移植）
// ============================================================

/// 引号状态机：只在引号位置推进（其余字节不影响状态）。
///
/// 与 JS 版逐字节语义一致：引号外 `"` 进入引号；引号内 `""` 为转义字面量，
/// 落单 `"` 退出引号。
struct QuoteMachine {
    in_quotes: bool,
}

impl QuoteMachine {
    fn new(in_quotes: bool) -> Self {
        Self { in_quotes }
    }

    /// 推进一个完整字节块。
    fn feed(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while let Some(rel) = memchr(b'"', &bytes[i..]) {
            let q = i + rel;
            if self.in_quotes && bytes.get(q + 1) == Some(&b'"') {
                i = q + 2; // "" 转义：消耗一对
            } else {
                self.in_quotes = !self.in_quotes;
                i = q + 1;
            }
        }
    }
}

/// 计算一段字节的引号状态迁移（false/true 两种初态各跑一遍）。
///
/// 宽松方言下奇数引号串的净效果依赖初态，无法只做奇偶计数，
/// 必须两种初态各模拟一遍。
fn chunk_transitions(bytes: &[u8]) -> (bool, bool) {
    let mut from_false = QuoteMachine::new(false);
    from_false.feed(bytes);
    let mut from_true = QuoteMachine::new(true);
    from_true.feed(bytes);
    (from_false.in_quotes, from_true.in_quotes)
}

/// 解析单条记录：slice 可能含引号内嵌换行/结尾换行符，取首条记录即止。
///
/// `text` 为记录切片解码后的文本；分隔/引号/换行均为 ASCII，先按字节定位
/// 结构字符，内容段按 UTF-8 边界整段 push_str，多字节字符零损坏。
///
/// # Parameters
/// - `text`: 记录切片解码后的文本（可含多行与结尾 \r\n）。
/// - `delimiter`: 字段分隔符（CSV 为 b','）。
///
/// # Returns
/// 字段数组（未做列数补齐——调用方按需 pad/truncate）。
fn parse_record(text: &str, delimiter: u8) -> Vec<String> {
    let b = text.as_bytes();
    let n = b.len();
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut content_start = 0usize;
    let mut i = 0usize;
    while i < n {
        let ch = b[i];
        if in_quotes {
            if ch == b'"' {
                if b.get(i + 1) == Some(&b'"') {
                    cur.push_str(&text[content_start..i]);
                    cur.push('"');
                    i += 2;
                    content_start = i;
                    continue;
                }
                // 闭引号：内容段收尾，引号本身不入内容
                cur.push_str(&text[content_start..i]);
                in_quotes = false;
                i += 1;
                content_start = i;
                continue;
            }
            i += 1; // 内容字节（含多字节序列与内嵌换行）
            continue;
        }
        if ch == b'"' {
            // 引号外：内容段收尾，引号本身不入内容
            cur.push_str(&text[content_start..i]);
            in_quotes = true;
            i += 1;
            content_start = i;
            continue;
        }
        if ch == delimiter {
            cur.push_str(&text[content_start..i]);
            fields.push(std::mem::take(&mut cur));
            i += 1;
            content_start = i;
            continue;
        }
        if ch == b'\r' || ch == b'\n' {
            cur.push_str(&text[content_start..i]);
            fields.push(std::mem::take(&mut cur));
            return fields; // 记录结束（忽略 slice 内其余字节）
        }
        i += 1;
    }
    cur.push_str(&text[content_start..n]);
    fields.push(cur);
    fields
}

// ============================================================
// 记录感知索引
// ============================================================

/// 每个 band 覆盖的行数（pass ③ 的并行粒度）。
const BAND_LINES: usize = 1 << 16;

/// 索引结果：`line_off` 为每个候选行的字节偏移（'\n' 之后，首元素恒 0）；
/// `rec_lines` 为记录起始行的下标（升序）。空行与引号续行不在其中。
pub struct RawIndex {
    pub line_off: Vec<u64>,
    pub rec_lines: Vec<u32>,
}

impl RawIndex {
    /// 记录总数（含表头记录，若存在；空文件为 0）。
    pub fn n_records(&self) -> usize {
        self.rec_lines.len()
    }

    /// 记录 r 的字节切片范围 [start, end)（end 不超文件尾）。
    pub fn record_span(&self, bytes_len: usize, r: usize) -> (usize, usize) {
        let start = self.line_off[self.rec_lines[r] as usize] as usize;
        let end = if r + 1 < self.rec_lines.len() {
            self.line_off[self.rec_lines[r + 1] as usize] as usize
        } else {
            bytes_len
        };
        (start, end)
    }
}

/// 行内容（去掉结尾 '\n'）是否全为 '\r'（parseCsv 视其为空物理行跳过）。
/// 首字节非 '\r' 即返回，常规文件几乎零开销。
#[inline]
fn line_is_blank(bytes: &[u8], start: usize, end_incl_nl: usize) -> bool {
    let nl_excl = if end_incl_nl > start && bytes[end_incl_nl - 1] == b'\n' {
        end_incl_nl - 1
    } else {
        end_incl_nl
    };
    bytes[start..nl_excl].iter().all(|&c| c == b'\r')
}

/// 顺序建索引（测试用黄金参照；生产走 `build_index`）。
#[cfg(test)]
fn build_index_seq(bytes: &[u8]) -> RawIndex {
    let n = bytes.len();
    let mut line_off: Vec<u64> = Vec::with_capacity(n / 32 + 2);
    line_off.push(0);
    for pos in memchr_iter(b'\n', bytes) {
        line_off.push((pos + 1) as u64);
    }
    let lines = line_off.len();
    let mut rec_lines: Vec<u32> = Vec::new();
    let mut m = QuoteMachine::new(false);
    for i in 0..lines {
        let start = line_off[i] as usize;
        let end = if i + 1 < lines { line_off[i + 1] as usize } else { n };
        let started_open = m.in_quotes; // 本行起始状态 = 上一行末状态
        m.feed(&bytes[start..end]);
        if !started_open && !line_is_blank(bytes, start, end) {
            rec_lines.push(i as u32);
        }
    }
    RawIndex { line_off, rec_lines }
}

/// 并行建索引（三趟，见模块头注释）。
pub fn build_index(bytes: &[u8]) -> RawIndex {
    let n = bytes.len();
    // ---- pass ①：行偏移（按字节分块并行 memchr，块间有序拼接） ----
    let workers = rayon::current_num_threads().max(1);
    let part = n / workers + 1;
    let parts: Vec<Vec<u64>> = (0..workers)
        .into_par_iter()
        .map(|w| {
            let s = (w * part).min(n);
            let e = ((w + 1) * part).min(n);
            if s >= e {
                return Vec::new();
            }
            memchr_iter(b'\n', &bytes[s..e])
                .map(|p| (s + p + 1) as u64)
                .collect()
        })
        .collect();
    let total = 1 + parts.iter().map(|v| v.len()).sum::<usize>();
    let mut line_off: Vec<u64> = Vec::with_capacity(total);
    line_off.push(0);
    for mut v in parts {
        line_off.append(&mut v);
    }
    let lines = line_off.len();

    // ---- pass ②：band 引号迁移表（band 按行号对齐） ----
    let nbands = lines.div_ceil(BAND_LINES).max(1);
    let trans: Vec<(bool, bool)> = (0..nbands)
        .into_par_iter()
        .map(|b| {
            let ls = b * BAND_LINES;
            let le = ((b + 1) * BAND_LINES).min(lines);
            let s = line_off[ls] as usize;
            let e = if le < lines { line_off[le] as usize } else { n };
            chunk_transitions(&bytes[s..e])
        })
        .collect();
    let mut band_state = vec![false; nbands + 1];
    for b in 0..nbands {
        let t = trans[b];
        band_state[b + 1] = if band_state[b] { t.1 } else { t.0 };
    }

    // ---- pass ③：逐行递推记录起始（每行恰属一个 band） ----
    let per_band: Vec<Vec<u32>> = (0..nbands)
        .into_par_iter()
        .map(|b| {
            let ls = b * BAND_LINES;
            let le = ((b + 1) * BAND_LINES).min(lines);
            let mut m = QuoteMachine::new(band_state[b]);
            let mut out: Vec<u32> = Vec::new();
            for i in ls..le {
                let start = line_off[i] as usize;
                let end = if i + 1 < lines { line_off[i + 1] as usize } else { n };
                let started_open = m.in_quotes;
                m.feed(&bytes[start..end]);
                if !started_open && !line_is_blank(bytes, start, end) {
                    out.push(i as u32);
                }
            }
            out
        })
        .collect();
    let mut rec_lines: Vec<u32> = Vec::with_capacity(
        per_band.iter().map(|v| v.len()).sum(),
    );
    for mut v in per_band {
        rec_lines.append(&mut v);
    }
    RawIndex { line_off, rec_lines }
}

// ============================================================
// 渐进索引：分块扫描
// ============================================================

/// 首屏同步索引目标：最多扫 1 MiB 或 1000 条记录（含表头），先让表格可用；
/// 剩余部分交给后台单线程继续（慢盘上首屏仍是百毫秒级，而不是等整文件）。
const INITIAL_INDEX_BYTES: usize = 1 << 20;

/// 首屏同步索引的记录数上限（含表头）。短行大文件据此在 1 MiB 前收手。
const INITIAL_INDEX_RECORDS: usize = 1000;

/// 后台索引分块：单线程顺序读，限制磁盘并发；每块发布一次新行，
/// 读路径（拖滚动条）随时能拿到刚扫过的部分。
const INDEX_CHUNK_BYTES: usize = 1 << 20;

/// 分块扫描游标：所有跨块状态都在这里，块边界可落在任意字节
/// （包括多字节字符/引号内换行中间——引号状态机只认 ASCII 引号，安全）。
#[derive(Clone, Copy)]
struct ScanCursor {
    /// 已扫描（已喂入引号状态机）到的字节位置。
    pos: usize,
    /// 当前物理行起始偏移（可能早于 pos，块从行中间开始时）。
    line_start: usize,
    /// 当前物理行号（对应 line_off 下标）。
    line_no: u32,
    /// pos 处的引号状态。
    in_quotes: bool,
    /// 当前物理行起始时的引号状态：true = 本行是上一条记录的续行，
    /// 该记录在行结束前都不完整（渐进可见性判据）。
    line_started_open: bool,
}

impl ScanCursor {
    /// 从文件头开始。
    fn new() -> Self {
        Self {
            pos: 0,
            line_start: 0,
            line_no: 0,
            in_quotes: false,
            line_started_open: false,
        }
    }
}

/// 扫描 [pos, pos+max_bytes) 内的完整物理行，返回新行偏移与记录起始行号。
/// `cur` 推进到块尾（可能停在行中间，状态由游标携带到下一块）。
///
/// 块尾无换行且未到 EOF 时不产出行/记录，只推进引号状态；到 EOF 时把
/// 无结尾换行的最后一行按完整记录处理（与 `build_index_seq` 一致）。
///
/// # Parameters
/// - `bytes`: 整个文件字节。
/// - `cur`: 扫描游标（跨块状态）。
/// - `max_bytes`: 本块最多推进的字节数（≥1）。
///
/// # Returns
/// `(新增行偏移, 新增记录起始行号)`。
fn scan_chunk(
    bytes: &[u8],
    cur: &mut ScanCursor,
    max_bytes: usize,
) -> (Vec<u64>, Vec<u32>) {
    let n = bytes.len();
    if cur.pos >= n {
        return (Vec::new(), Vec::new());
    }
    let end = cur.pos.saturating_add(max_bytes.max(1)).min(n);
    let mut line_off: Vec<u64> = Vec::new();
    let mut rec_lines: Vec<u32> = Vec::new();
    let mut m = QuoteMachine::new(cur.in_quotes);
    let mut from = cur.pos;
    while let Some(rel) = memchr(b'\n', &bytes[from..end]) {
        let q = from + rel;
        // 行结束位置 = 换行符之后；本行起始时的引号状态决定是否记录起始。
        m.feed(&bytes[from..=q]);
        if !cur.line_started_open && !line_is_blank(bytes, cur.line_start, q + 1) {
            rec_lines.push(cur.line_no);
        }
        line_off.push((q + 1) as u64);
        cur.line_no += 1;
        cur.line_start = q + 1;
        cur.line_started_open = m.in_quotes;
        from = q + 1;
    }
    if from < end {
        // 块尾残行/块内无换行的超长行：只推进引号状态，行完成留给后续块。
        m.feed(&bytes[from..end]);
    }
    cur.in_quotes = m.in_quotes;
    cur.pos = end;
    if end == n && cur.line_start < n {
        // 文件末行无换行：同样是一条完整记录，但不产生新行偏移。
        if !cur.line_started_open && !line_is_blank(bytes, cur.line_start, n) {
            rec_lines.push(cur.line_no);
        }
        cur.line_start = n;
        cur.line_started_open = false;
    }
    (line_off, rec_lines)
}

/// 当前已完整、可安全展示的记录数。
///
/// 未完成时若当前物理行是上一条记录的续行（`line_started_open`），该记录
/// 的结束位置还未知，必须暂时隐藏；否则已产出的记录都已在新行边界结束。
///
/// # Parameters
/// - `n`: 已产出的记录数。
/// - `line_started_open`: 当前未完成物理行是否从引号内开始。
/// - `done`: 是否已扫到 EOF。
///
/// # Returns
/// 可展示的记录数（含表头记录）。
fn usable_record_count(n: usize, line_started_open: bool, done: bool) -> usize {
    if done || !line_started_open {
        n
    } else {
        n.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进度节流：大间隔时首次放行、紧接着的突发调用全部被压住，
    /// 零间隔（不节流路径）行为不受影响。
    #[test]
    fn progress_gate_throttles_bursts() {
        let mut last: Option<Instant> = None;
        assert!(progress_gate(&mut last, Duration::from_secs(3600)));
        assert!(!progress_gate(&mut last, Duration::from_secs(3600)));
        assert!(!progress_gate(&mut last, Duration::from_secs(3600)));
        let mut always: Option<Instant> = None;
        assert!(progress_gate(&mut always, Duration::from_millis(0)));
        assert!(progress_gate(&mut always, Duration::from_millis(0)));
    }

    /// 进度节流：间隔过后重新放行。
    #[test]
    fn progress_gate_reopens_after_gap() {
        let mut last: Option<Instant> = None;
        assert!(progress_gate(&mut last, Duration::from_millis(20)));
        std::thread::sleep(Duration::from_millis(30));
        assert!(progress_gate(&mut last, Duration::from_millis(20)));
    }

    /// 低优先级守卫：进入后当前线程优先级变为 BELOW_NORMAL，drop 后
    /// 必须恢复到进入前的值——优先级泄漏会让后续无关任务长期低优先级运行。
    #[cfg(windows)]
    #[test]
    fn background_guard_roundtrip() {
        use std::ffi::c_void;
        type Handle = *mut c_void;
        extern "system" {
            fn GetCurrentThread() -> Handle;
            fn GetThreadPriority(h: Handle) -> i32;
        }
        unsafe {
            let h = GetCurrentThread();
            let before = GetThreadPriority(h);
            {
                let _g = bg::BackgroundGuard::enter();
                let during = GetThreadPriority(h);
                assert_eq!(during, -1, "进入后应为 BELOW_NORMAL(-1)，实际 {during}");
            }
            let after = GetThreadPriority(h);
            assert_eq!(after, before, "drop 后必须恢复进入前的优先级 {before}，实际 {after}");
        }
    }

    /// 嗅探头部截断在多字节字符中间时仍应判 UTF-8（65536 边界恰好切分
    /// 3 字节字符的回归：from_utf8 报 incomplete → 曾误判 GBK 全表乱码）。
    #[test]
    fn sniff_head_mid_char_boundary() {
        // "大理,3,古城\r\n" × N：17 字节/行，65536 = 17×3855 + 1，尾字节属下一字符
        let mut v = Vec::new();
        for _ in 0..4000 {
            v.extend_from_slice("大理,3,古城\r\n".as_bytes());
        }
        assert_eq!(v.len() > 65536, true);
        assert!(matches!(sniff_encoding(&v), Encoding::Utf8));
    }

    /// 引号串语义对照 JS parseCsv 逐字节推演。
    #[test]
    fn quote_runs_trace() {
        // 3 个引号（块外）：进、""对、净 = 在引号内
        let mut m = QuoteMachine::new(false);
        m.feed(b"\"\"\"");
        assert!(m.in_quotes);
        // 2 个引号（块外）：进、出
        let mut m = QuoteMachine::new(false);
        m.feed(b"\"\"");
        assert!(!m.in_quotes);
        // 5 个（块外）：进 + 两对 = 在引号内
        let mut m = QuoteMachine::new(false);
        m.feed(b"\"\"\"\"\"");
        assert!(m.in_quotes);
        // 4 个（块外）：进 + 对 + 出 = 出
        let mut m = QuoteMachine::new(false);
        m.feed(b"\"\"\"\"");
        assert!(!m.in_quotes);
        // 空块 = 恒等迁移
        assert_eq!(chunk_transitions(b""), (false, true));
        // 奇数串块内外效果不同 → 两初态必须各算
        assert_eq!(chunk_transitions(b"\"\"\""), (true, false));
        // x,"a""b"：块外出、块内末态留在引号内
        assert_eq!(chunk_transitions(b"x,\"a\"\"b\""), (false, true));
    }

    #[test]
    fn parse_record_basics() {
        assert_eq!(parse_record("a,b,c", b','), vec!["a", "b", "c"]);
        assert_eq!(parse_record("a,b,c\r\n", b','), vec!["a", "b", "c"]);
        assert_eq!(parse_record("\"x,y\",z", b','), vec!["x,y", "z"]);
        assert_eq!(parse_record("\"a\"\"b\",c", b','), vec!["a\"b", "c"]);
        // 宽松方言：引号可在字段中间出现（字段内容 = 引号间拼接）
        assert_eq!(parse_record("a\"b\"c,d", b','), vec!["abc", "d"]);
        // 内嵌换行（多行 slice 取首条记录）
        assert_eq!(
            parse_record("\"line1\nline2\",next\nlater", b','),
            vec!["line1\nline2", "next"]
        );
        // 多字节 UTF-8 内容段
        assert_eq!(parse_record("古城,丽江", b','), vec!["古城", "丽江"]);
        // 单字段无分隔符
        assert_eq!(parse_record("solo", b','), vec!["solo"]);
        assert_eq!(parse_record("", b','), vec![""]);
    }

    fn rec_starts_of(bytes: &[u8]) -> Vec<usize> {
        let idx = build_index(bytes);
        let seq = build_index_seq(bytes);
        assert_eq!(
            idx.rec_lines, seq.rec_lines,
            "并行索引与顺序参照不一致"
        );
        idx.rec_lines.iter().map(|&l| l as usize).collect()
    }

    #[test]
    fn index_plain() {
        // 3 行 + 表头
        assert_eq!(rec_starts_of(b"a,b\n1,2\n3,4\n"), vec![0, 1, 2]);
        // 无结尾换行
        assert_eq!(rec_starts_of(b"a,b\n1,2"), vec![0, 1]);
        // 空文件
        assert_eq!(rec_starts_of(b""), vec![] as Vec<usize>);
        // 仅表头
        assert_eq!(rec_starts_of(b"a,b\n"), vec![0]);
        // CRLF 行尾
        assert_eq!(rec_starts_of(b"a,b\r\n1,2\r\n"), vec![0, 1]);
    }

    #[test]
    fn index_blank_lines() {
        // 中间空行被跳过（parseCsv 语义）
        assert_eq!(rec_starts_of(b"a,b\n1,2\n\n3,4\n"), vec![0, 1, 3]);
        // 连续多个空行
        assert_eq!(rec_starts_of(b"a,b\n\n\n\n1,2\n"), vec![0, 4]);
        // 文件尾多余空行不产生记录
        assert_eq!(rec_starts_of(b"a,b\n1,2\n\n"), vec![0, 1]);
    }

    #[test]
    fn index_embedded_newlines() {
        // 引号内嵌换行：物理 3 行，记录 2 条（表头 + 数据）
        assert_eq!(rec_starts_of(b"h1,h2\n\"a\nb\",c\nd,e\n"), vec![0, 1, 3]);
        // 内嵌换行紧跟前一行（第二行是续行）
        assert_eq!(rec_starts_of(b"h1,h2\n\"a\nb\"\n"), vec![0, 1]);
        // 引号跨行再闭合后，后续行恢复正常
        assert_eq!(
            rec_starts_of(b"h\n\"x\ny\"\nz\n\"p\nq\"\nr\n"),
            vec![0, 1, 3, 4, 6]
        );
    }

    #[test]
    fn index_lenient_quotes() {
        // 宽松方言：字段中间引号（不成对也不跨行）
        assert_eq!(rec_starts_of(b"h1,h2\na\"b,c\n"), vec![0, 1]);
        // 成对字段引号
        assert_eq!(rec_starts_of(b"h1,h2\n\"a\",b\n"), vec![0, 1]);
    }

    #[test]
    fn index_span_fetch() {
        let bytes = b"h1,h2\n\"a\nb\",c\nd,e\n";
        let idx = build_index(bytes);
        let (s, e) = idx.record_span(bytes.len(), 1);
        assert_eq!(&bytes[s..e], b"\"a\nb\",c\n");
        let (s, e) = idx.record_span(bytes.len(), 2);
        assert_eq!(&bytes[s..e], b"d,e\n");
    }

    // ---- 渐进索引：分块扫描 / 部分可见性 / 打开生命周期 ----

    /// 用 `scan_chunk` 按指定块大小扫完全文，拼出索引（黄金参照对拍用）。
    fn incremental_index(bytes: &[u8], chunk: usize) -> RawIndex {
        let mut cur = ScanCursor::new();
        let mut line_off = vec![0u64];
        let mut rec_lines = Vec::new();
        loop {
            let (o, r) = scan_chunk(bytes, &mut cur, chunk);
            line_off.extend(o);
            rec_lines.extend(r);
            if cur.pos >= bytes.len() {
                break;
            }
        }
        RawIndex { line_off, rec_lines }
    }

    /// 任意小块切分都必须与顺序索引逐字节一致：块尾不完整行/引号状态跨块、
    /// CRLF、空行、无结尾换行、宽松引号全部覆盖。
    #[test]
    fn scan_chunk_matches_seq_for_all_chunk_sizes() {
        let cases: Vec<Vec<u8>> = vec![
            b"a,b\n1,2\n3,4\n".to_vec(),
            b"a,b\r\n1,2\r\n3,4\r\n".to_vec(),
            b"a,b\n1,2\n3,4".to_vec(),
            b"a,b\n1,2\n\n3,4\n".to_vec(),
            b"a,b\n\n\n\n1,2\n\n".to_vec(),
            b"h1,h2\n\"a\nb\",c\nd,e\n".to_vec(),
            b"h\n\"x\ny\"\nz\n\"p\nq\"\nr\n".to_vec(),
            b"h1,h2\na\"b,c\n\"a\",b\n".to_vec(),
            b"h\n\"unterminated\n".to_vec(),
            b"".to_vec(),
            b"\n".to_vec(),
            "标题,备注\n\"第一行\n第二行\",值\n第三行,尾\n".as_bytes().to_vec(),
        ];
        for bytes in cases {
            let golden = build_index_seq(&bytes);
            for &chunk in &[1usize, 2, 3, 5, 7, 13, 64, 1 << 20] {
                let got = incremental_index(&bytes, chunk);
                assert_eq!(
                    got.line_off, golden.line_off,
                    "line_off 不一致：chunk={chunk} bytes={:?}",
                    String::from_utf8_lossy(&bytes)
                );
                assert_eq!(
                    got.rec_lines, golden.rec_lines,
                    "rec_lines 不一致：chunk={chunk} bytes={:?}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }

    /// 不完整的引号续行必须隐藏最后一条记录；尚未产出记录的新行不能把
    /// 上一条完整记录误藏（header 不能被吞）。
    #[test]
    fn usable_hides_partial_continuation_record() {
        let bytes = b"h\n\"a\nb\"\n";
        let mut cur = ScanCursor::new();
        let mut recs: Vec<u32> = Vec::new();

        let (_, r) = scan_chunk(bytes, &mut cur, 3); // 恰好停在 `"` 之后
        recs.extend(r);
        assert_eq!(recs, vec![0], "header 已完整");
        assert!(!cur.line_started_open, "新记录尚未开始（当前行从引号外起）");
        assert_eq!(
            usable_record_count(recs.len(), cur.line_started_open, false),
            1,
            "尚未产出的记录不能把 header 一起藏掉"
        );

        let (_, r) = scan_chunk(bytes, &mut cur, 3); // 停在续行 `b` 之后
        recs.extend(r);
        assert_eq!(recs, vec![0, 1], "多行记录第一行已完成、已产出");
        assert!(cur.line_started_open, "当前行是上一条记录的续行");
        assert_eq!(
            usable_record_count(recs.len(), cur.line_started_open, false),
            1,
            "续行未结束：最后一条记录必须暂时隐藏"
        );

        let (_, r) = scan_chunk(bytes, &mut cur, 3);
        recs.extend(r);
        let done = cur.pos >= bytes.len();
        assert!(done);
        assert_eq!(
            usable_record_count(recs.len(), cur.line_started_open, done),
            2,
            "EOF 后全部记录可见"
        );
    }

    /// 渐进打开：前缀立即可读、结构操作/筛选/保存被拒；补齐后行数增长、
    /// 索引期单元格编辑经覆盖层保留。
    #[test]
    fn progressive_open_prefix_and_refresh() {
        let mut s = String::from("a,b\r\n");
        for i in 0..100 {
            s.push_str(&format!("{i:04},x\r\n"));
        }
        let bytes = s.into_bytes();
        // 表头 5B + 3 行×8B = 29B：前缀恰好 3 条数据行
        let p = tmp_csv("progressive", &bytes);
        let mut bf =
            BigFile::open_progressive_with(&p, 29, 1000, 8, false).unwrap();

        let m = bf.meta();
        assert!(m.indexing);
        assert_eq!(m.rows, 3, "前缀应立即可用 3 行");
        assert_eq!(m.total_bytes, bytes.len() as u64);
        assert!(m.indexed_bytes < m.total_bytes);
        assert_eq!(m.index_ms, 0, "索引未完成时 index_ms 为 0");
        assert!(m.open_ms <= m.elapsed_ms);

        let rows = bf.rows(0, 100);
        assert_eq!(rows.len(), 3, "未索引部分不得返回");
        assert_eq!(rows[2], vec!["0002", "x"]);

        // 索引期允许仿编辑，但结构变更/筛选/保存/列操作必须拒绝
        bf.set_cell(2, 1, "EDITED".into()).unwrap();
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED");
        assert!(bf.insert_rows(0, None, 1).is_err());
        assert!(bf.remove_rows(vec![0]).is_err());
        assert!(bf.run_filter(&FilterSpec::default()).is_err());
        assert!(bf.save(|_, _, _, _| {}).is_err());
        assert!(bf
            .push_col_op(ColOp::Rename { col: 0, name: "A".into() })
            .is_err());

        // 后台补齐（测试里同步驱动），完成瞬间无需 refresh 也能读全量
        bf.drive_index_for_test();
        let m = bf.meta();
        assert!(!m.indexing);
        assert_eq!(m.rows, 100);
        assert!(m.index_ms <= m.elapsed_ms);
        assert_eq!(bf.rows(0, 1000).len(), 100);
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED", "覆盖层跨索引完成保留");

        // 并入正式索引后行为不变，且重复调用幂等
        bf.refresh_indexed();
        assert!(bf.partial.is_none());
        bf.refresh_indexed();
        assert_eq!(bf.meta().rows, 100);
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED");
        assert!(bf.insert_rows(100, None, 1).is_ok(), "完成后恢复结构操作");

        fs::remove_file(&p).ok();
    }

    /// 真实后台线程：打开立即返回，随后行数增长至全量。
    #[test]
    fn progressive_open_background_completes() {
        let mut s = String::from("a,b\r\n");
        for i in 0..300 {
            s.push_str(&format!("{i:04},x\r\n"));
        }
        let bytes = s.into_bytes();
        let p = tmp_csv("progressive_bg", &bytes);
        // 前缀 64B：表头 + 7 行；后台每块 64B
        let mut bf = BigFile::open_progressive_with(&p, 64, 3, 64, true).unwrap();
        assert!(bf.meta().indexing);
        let deadline = Instant::now() + Duration::from_secs(5);
        while bf.meta().indexing && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!bf.meta().indexing, "后台索引未在 5 秒内完成");
        assert_eq!(bf.meta().rows, 300);
        bf.refresh_indexed();
        assert_eq!(bf.rows(0, 1000).len(), 300);
        assert_eq!(bf.rows(299, 1)[0][0], "0299");
        fs::remove_file(&p).ok();
    }

    // ---- Part 3：BigFile 行为测试（临时文件夹具） ----

    /// 写临时 CSV 夹具并返回路径（进程号隔离，避免并行测试撞名）。
    fn tmp_csv(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join(format!("st_engine_{}_{}.csv", name, std::process::id()));
        fs::write(&p, bytes).unwrap();
        p
    }

    /// 五数据行夹具：name,age,city。
    fn fixture_people() -> Vec<Vec<&'static str>> {
        vec![
            vec!["Alice", "30", "Beijing"],
            vec!["bob", "25", "Shanghai"],
            vec!["Carol", "35", "Beijing"],
            vec!["dave", "28", "Guangzhou"],
            vec!["Eve", "22", "Shanghai"],
        ]
    }

    fn people_csv() -> Vec<u8> {
        let mut s = String::from("name,age,city\r\n");
        for r in fixture_people() {
            s.push_str(&r.join(","));
            s.push_str("\r\n");
        }
        s.into_bytes()
    }

    #[test]
    fn big_open_meta_strips_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(&people_csv());
        let p = tmp_csv("bom", &bytes);
        let bf = BigFile::open(&p).unwrap();
        let m = bf.meta();
        assert_eq!(m.rows, 5);
        assert_eq!(m.cols, 3);
        assert_eq!(m.headers, vec!["name", "age", "city"]);
        assert_eq!(m.encoding, "utf-8");
        assert!(!m.pending_cols && !m.can_undo && !m.can_redo);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_open_rejects_empty_and_header_only() {
        let p = tmp_csv("empty", b"");
        assert!(BigFile::open(&p).is_err());
        fs::remove_file(&p).ok();
        // 仅表头 = 0 数据行，合法（与小模式一致，允许空表）
        let p = tmp_csv("hdronly", b"a,b\n");
        let bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().rows, 0);
        assert_eq!(bf.meta().headers, vec!["a", "b"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_rows_window_and_embedded_newline() {
        // 第二条记录含引号内换行，应视为一行
        let bytes = b"name,note\r\nAlice,\"line1\nline2\"\r\nbob,x\r\n";
        let p = tmp_csv("nl", bytes);
        let bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().rows, 2);
        let all = bf.rows(0, 10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], vec!["Alice", "line1\nline2"]);
        assert_eq!(all[1], vec!["bob", "x"]);
        // 窗口分页与全量一致
        assert_eq!(bf.rows(1, 1), vec![vec!["bob", "x"]]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_set_cell_undo_redo_src_and_ins() {
        let p = tmp_csv("cell", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 源行编辑走覆盖层
        bf.set_cell(1, 0, "BOB".into()).unwrap();
        assert_eq!(bf.rows(1, 1)[0][0], "BOB");
        assert!(bf.meta().can_undo);
        bf.undo();
        assert_eq!(bf.rows(1, 1)[0][0], "bob");
        bf.redo();
        assert_eq!(bf.rows(1, 1)[0][0], "BOB");
        // 撤销后再改 + 重做栈被清
        bf.undo();
        bf.set_cell(1, 0, "b2".into()).unwrap();
        assert!(!bf.meta().can_redo);
        // 插入行直接写块
        bf.insert_rows(0, Some(vec![vec!["X".into(), "1".into(), "Y".into()]]), 0)
            .unwrap();
        bf.set_cell(0, 1, "99".into()).unwrap();
        assert_eq!(bf.rows(0, 1)[0][1], "99");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_insert_remove_undo_restores_order() {
        let p = tmp_csv("insdel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 在第 2 行前插入两行（带值 + 空行）
        bf.insert_rows(2, Some(vec![vec!["NEW".into(), "1".into(), "Z".into()]]), 0)
            .unwrap();
        bf.insert_rows(3, None, 1).unwrap();
        assert_eq!(bf.meta().rows, 7);
        assert_eq!(bf.rows(2, 2)[0][0], "NEW");
        assert_eq!(bf.rows(3, 1)[0], vec!["", "", ""]);
        // 删：一个插入块行 + 一个源行（原第 0 行 Alice 现为 vrow 0）
        assert_eq!(bf.remove_rows(vec![3, 0]).unwrap(), 2);
        assert_eq!(bf.meta().rows, 5);
        assert_eq!(bf.rows(0, 5)[0][0], "bob"); // Alice 被删
        // 撤销删除 → 顺序完整恢复
        bf.undo();
        assert_eq!(bf.meta().rows, 7);
        let all = bf.rows(0, 7);
        assert_eq!(all[0][0], "Alice");
        assert_eq!(all[2][0], "NEW");
        assert_eq!(all[3], vec!["", "", ""]);
        // 撤销插入（逆序）
        bf.undo();
        bf.undo();
        assert_eq!(bf.meta().rows, 5);
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_remove_redo_descending_safe() {
        let p = tmp_csv("redodel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 连续删前两行再重做：升序删除会因位移错位，引擎须内部降序
        bf.remove_rows(vec![0, 1]).unwrap();
        assert_eq!(bf.rows(0, 2)[0][0], "Carol");
        bf.undo();
        assert_eq!(bf.rows(0, 2)[0][0], "Alice");
        bf.redo();
        assert_eq!(bf.rows(0, 2)[0][0], "Carol");
        assert_eq!(bf.rows(1, 1)[0][0], "dave");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_remove_redo_covers_ins_rows() {
        let p = tmp_csv("redoins", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.insert_rows(1, Some(vec![vec!["TMP".into(), "0".into(), "X".into()]]), 0)
            .unwrap();
        // 删源行(0) + 删插入行(1)，undo 后 redo：两类行都必须重新消失
        bf.remove_rows(vec![0, 1]).unwrap();
        assert_eq!(bf.meta().rows, 4);
        bf.undo();
        assert_eq!(bf.meta().rows, 6);
        assert_eq!(bf.rows(1, 1)[0][0], "TMP");
        bf.redo();
        assert_eq!(bf.meta().rows, 4);
        assert_eq!(bf.rows(0, 2)[0][0], "bob");
        assert_eq!(bf.rows(1, 1)[0][0], "Carol");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_filter_and_multi_or_semantics() {
        let p = tmp_csv("filter", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 单列 AND：city 含 "beijing"（小写匹配）
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(2, "beijing".into())],
                multi_cols: vec![],
                multi_text: String::new(),
            })
            .unwrap();
        assert_eq!(n, 2); // Alice + Carol
        // 双列 AND：name 含 "a" 且 city 含 "beijing" → Alice + Carol
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(0, "a".into()), (2, "beijing".into())],
                multi_cols: vec![],
                multi_text: String::new(),
            })
            .unwrap();
        assert_eq!(n, 2);
        // 双列 AND 无交集 → 0
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(0, "a".into()), (2, "shanghai".into())],
                multi_cols: vec![],
                multi_text: String::new(),
            })
            .unwrap();
        assert_eq!(n, 0); // name 含 a 的是 Alice/Carol/dave，都不在 Shanghai
        // 多重筛选 OR：name 或 city 含 "carol"
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![],
                multi_cols: vec![0, 2],
                multi_text: "carol".into(),
            })
            .unwrap();
        assert_eq!(n, 1);
        // 命中页
        let page = bf.filter_page(0, 10);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0][0], "Carol");
        assert!(bf.has_filter());
        // 筛选结果对源行编辑立即生效（覆盖层参与）
        bf.set_cell(2, 0, "carolina".into()).unwrap();
        let page = bf.filter_page(0, 10);
        assert_eq!(page[0][0], "carolina");
        // 清除
        bf.clear_filter();
        assert!(!bf.has_filter());
        fs::remove_file(&p).ok();
    }


    #[test]
    fn big_copy_tsv_escapes() {
        let bytes = b"a,b\r\n\"x,y\",\"l1\nl2\"\r\nplain,ok\r\n";
        let p = tmp_csv("tsv", bytes);
        let bf = BigFile::open(&p).unwrap();
        // TSV 语义：仅 TAB/引号/换行触发引号（逗号不触发，对齐前端 serializeRecords）
        let tsv = bf.copy_tsv(0, 2, 0, 2).unwrap();
        assert_eq!(tsv, "x,y\t\"l1\nl2\"\r\nplain\tok\r\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_roundtrip_and_keeps_undo() {
        let p = tmp_csv("save", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into()).unwrap(); // Alice age 30→31
        bf.insert_rows(1, Some(vec![vec!["Zed".into(), "1".into(), "X".into()]]), 0)
            .unwrap();
        bf.remove_rows(vec![3]).unwrap(); // 删 Carol（插入位移后 vrow 3）
        let mut prog = 0u32;
        bf.save(|done, _total, _phase, _rows| prog = prog.max(done as u32))
            .unwrap();
        assert!(prog > 0, "进度回调应至少触发一次");
        assert!(bf.meta().can_undo, "保存后撤销栈保留（rec 稳定）");
        // 重开比对物理内容
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.meta().rows, 5);
        let all = bf2.rows(0, 5);
        assert_eq!(all[0], vec!["Alice", "31", "Beijing"]);
        assert_eq!(all[1], vec!["Zed", "1", "X"]);
        assert_eq!(all[2], vec!["bob", "25", "Shanghai"]);
        assert_eq!(all[3], vec!["dave", "28", "Guangzhou"]);
        assert_eq!(all[4], vec!["Eve", "22", "Shanghai"]);
        // 保存后仍可撤销编辑（覆盖层语义在写入后依然成立）
        bf.undo();
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_gbk_reencodes() {
        let (enc, _, _) = encoding_rs::GBK.encode("名字,备注\r\n张三,北京\r\n");
        let p = tmp_csv("gbk", &enc.into_owned());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().encoding, "gbk");
        bf.set_cell(0, 1, "上海".into()).unwrap();
        bf.save(|_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.meta().encoding, "gbk");
        assert_eq!(bf2.rows(0, 1)[0], vec!["张三", "上海"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_passthrough_preserves_raw_bytes() {
        // 字节透传契约：未触碰的行在保存后必须逐字节等于原文件
        // （引号风格、多余空格、内嵌换行、末尾无换行全部保留——
        // 旧实现整表重编码会把 `"Bob "` 的引号剥掉、给末行补 \r\n）。
        let content = b"name,note\r\n\"Alice\",\"line1\nline2\"\r\n\"Bob \",\" spaced \"\r\nCarol,plain\r\nDave,x";
        let p = tmp_csv("passthru", content);
        let mut bf = BigFile::open(&p).unwrap();
        bf.remove_rows(vec![2]).unwrap(); // 删 Carol（中间行）
        bf.save(|_, _, _, _| {}).unwrap();
        let out = fs::read(&p).unwrap();
        let expected: &[u8] = b"name,note\r\n\"Alice\",\"line1\nline2\"\r\n\"Bob \",\" spaced \"\r\nDave,x";
        assert_eq!(out, expected, "未触碰行必须逐字节保留");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_passthrough_mixed_edit_delete_insert() {
        // 混合改动：删除的行消失、编辑的行重编码、插入的行编码、
        // 其余行透传（末行无换行时透传段不补换行）。
        let content = b"name,age\r\nAlice,30\r\nBob,40\r\nCarol,50";
        let p = tmp_csv("passthru2", content);
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(1, 1, "41".into()).unwrap(); // Bob age 40→41
        bf.remove_rows(vec![0]).unwrap(); // 删 Alice
        bf.insert_rows(1, Some(vec![vec!["Zed".into(), "1".into()]]), 0)
            .unwrap(); // vrow 1 插 Zed（Bob 之后）
        bf.save(|_, _, _, _| {}).unwrap();
        let out = fs::read(&p).unwrap();
        let expected: &[u8] = b"name,age\r\nBob,41\r\nZed,1\r\nCarol,50";
        assert_eq!(out, expected, "混合改动后的物理字节");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_col_ops_pending_blocks_edits_then_apply() {
        let p = tmp_csv("colops", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // pending：视图立即多出列
        bf.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() }).unwrap();
        assert!(bf.meta().pending_cols);
        assert_eq!(bf.meta().cols, 4);
        assert_eq!(bf.meta().headers, vec!["name", "memo", "age", "city"]);
        // pending 期间编辑/筛选被拒
        assert!(bf.set_cell(0, 0, "x".into()).is_err());
        assert!(bf.run_filter(&FilterSpec {
            cols: vec![(0, "a".into())],
            multi_cols: vec![],
            multi_text: String::new(),
        }).is_err());
        // 撤销 pending（pop）后恢复
        bf.pop_col_op();
        assert!(!bf.meta().pending_cols);
        assert_eq!(bf.meta().cols, 3);
        // 重新 push 并真正执行
        bf.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() }).unwrap();
        bf.push_col_op(ColOp::Rename { col: 0, name: "姓名".into() }).unwrap();
        bf.push_col_op(ColOp::Delete { col: 2 }).unwrap(); // 删原 age
        bf.apply_col_ops(|_, _, _, _| {}).unwrap();
        assert!(!bf.meta().pending_cols);
        assert_eq!(bf.meta().headers, vec!["姓名", "memo", "city"]);
        assert!(!bf.meta().can_undo, "列操作物理重写后清空撤销栈");
        // 物理落盘验证
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.meta().headers, vec!["姓名", "memo", "city"]);
        assert_eq!(bf2.rows(0, 1)[0], vec!["Alice", "", "Beijing"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_col_ops_progress_phased_monotonic() {
        // 进度契约（进度条配平，治"中途定住像卡死"）：
        // 阶段 0 流式重写按行推进（加权到 70%），阶段 1 落盘换名（70→78%），
        // 阶段 2 重建索引完成后收尾必到满 (total,total)；序列单调不减。
        let p = tmp_csv("colprog", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.push_col_op(ColOp::Delete { col: 2 }).unwrap();
        let mut events: Vec<(u64, u64, u8, u64)> = Vec::new();
        bf.apply_col_ops(|d, t, ph, rows| events.push((d, t, ph, rows)))
            .unwrap();
        assert!(!events.is_empty(), "至少应收到收尾进度事件");
        for w in events.windows(2) {
            assert!(w[1].0 >= w[0].0, "进度应单调不减：{:?} -> {:?}", w[0], w[1]);
        }
        assert_eq!(events[0].2, PHASE_STREAM, "首事件应为流式阶段：{:?}", events);
        assert!(
            events.iter().any(|e| e.2 == PHASE_FLUSH),
            "应有落盘阶段事件：{:?}",
            events
        );
        let last = events.last().unwrap();
        assert_eq!(last.0, last.1, "收尾应到满：{:?}", last);
        assert_eq!(last.2, PHASE_REINDEX, "收尾应标记索引阶段：{:?}", last);
        assert!(last.3 > 0, "行计数应非零：{:?}", last);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_stale_detects_external_change() {
        let p = tmp_csv("stale", &people_csv());
        let bf = BigFile::open(&p).unwrap();
        assert!(!bf.stale());
        // 外部追加（模拟其他程序改动）
        let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write as _;
        f.write_all(b"Zoe,9,Nowhere\r\n").unwrap();
        drop(f);
        assert!(bf.stale(), "mtime/大小变化后应判脏");
        fs::remove_file(&p).ok();
    }

    /// Windows：外部程序独占打开原文件（share_mode=0，如 Excel/预览窗格/杀软扫描中的句柄）
    /// 时换名必然失败——错误必须告知临时文件位置（新内容未丢），且会话映回原文件继续可用。
    #[cfg(windows)]
    #[test]
    fn save_rename_blocked_keeps_tmp_and_session_usable() {
        use std::os::windows::fs::OpenOptionsExt;
        let p = tmp_csv("saveblock", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "Bob".into()).unwrap();
        // 模拟外部程序占用：读共享但拒绝删除共享（FILE_SHARE_READ|WRITE=0x3，
        // 同 Excel/资源管理器预览类句柄），rename 必失败（os error 32）
        let lock = fs::OpenOptions::new().read(true).share_mode(0x3).open(&p).unwrap();
        let err = bf.save(|_, _, _, _| {}).unwrap_err();
        drop(lock);
        assert!(err.contains("临时文件"), "错误应告知临时文件位置: {err}");
        let tmp = tmp_path_for(&p);
        assert!(tmp.exists(), "写好的新文件必须保留");
        assert!(fs::metadata(&tmp).unwrap().len() > 0);
        // 会话必须仍可用：覆盖层/撤销栈未丢，读路径已映回原文件
        assert_eq!(bf.rows(0, 1)[0][0], "Bob");
        assert!(bf.undo(), "撤销栈应保留");
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        fs::remove_file(&tmp).ok();
        fs::remove_file(&p).ok();
    }

    /// Windows：占用是瞬时的情况下（杀软/索引器），重试应能等到锁释放并成功换名。
    #[cfg(windows)]
    #[test]
    fn save_rename_recovers_when_lock_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let p = tmp_csv("saveretry", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "Bob".into()).unwrap();
        // 读共享占用 400ms 后释放；rename 重试窗口（5 次 × 200ms）应覆盖到
        let lock = fs::OpenOptions::new().read(true).share_mode(0x3).open(&p).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            drop(lock);
        });
        bf.save(|_, _, _, _| {}).unwrap();
        t.join().unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0][0], "Bob", "新文件应落盘生效");
        fs::remove_file(&p).ok();
    }

    // ---- Part 4：大文件秒筛（字节路径 + 列投影缓存 + 收窄 + 中断） ----

    fn spec_one(col: u32, kw: &str) -> FilterSpec {
        FilterSpec {
            cols: vec![(col, kw.into())],
            multi_cols: vec![],
            multi_text: String::new(),
        }
    }

    fn spec_multi(cols: Vec<u32>, kw: &str) -> FilterSpec {
        FilterSpec {
            cols: vec![],
            multi_cols: cols,
            multi_text: kw.into(),
        }
    }

    /// 引号（逗号/双引号转义）、内嵌换行、大小写混合：字节快路径语义必须
    /// 与 decode+parse_record 的慢路径完全一致。
    #[test]
    fn big_filter_fast_path_quoted_and_multiline() {
        let s = "name,note,city\r\n\
                 ALICE,\"hello, world\",BEIJING\r\n\
                 bob,\"line1\nline2\",Shanghai\r\n\
                 carol,\"say \"\"hi\"\"\",BEIJING\r\n\
                 dave,plain,Guangzhou\r\n";
        let p = tmp_csv("fastq", s.as_bytes());
        let mut bf = BigFile::open(&p).unwrap();

        // 大写数据 + 小写关键字（ASCII 折叠）
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        // 引号内的逗号不能当列分隔符
        assert_eq!(bf.run_filter(&spec_one(1, "hello, world")).unwrap(), 1);
        // 双引号转义：解析后的值应为 say "hi"
        assert_eq!(bf.run_filter(&spec_one(1, "say \"hi\"")).unwrap(), 1);
        // 引号内换行属于同一记录
        assert_eq!(bf.run_filter(&spec_one(1, "line1")).unwrap(), 1);
        assert_eq!(bf.run_filter(&spec_one(1, "line2")).unwrap(), 1);
        // 第一列
        assert_eq!(bf.run_filter(&spec_one(0, "alice")).unwrap(), 1);
        // 多重筛选 OR
        assert_eq!(bf.run_filter(&spec_multi(vec![0, 2], "carol")).unwrap(), 1);

        let page = bf.filter_page(0, 10);
        assert_eq!(page[0][0], "carol");
        fs::remove_file(&p).ok();
    }

    /// UTF-8 中文关键字（非 ASCII 走解码回退）与中文列上的 ASCII 关键字。
    #[test]
    fn big_filter_utf8_cjk_and_ascii() {
        let s = "城市,备注\n北京,ABC\n上海,def\n北京,GHI\n";
        let p = tmp_csv("fastutf8", s.as_bytes());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.run_filter(&spec_one(0, "北京")).unwrap(), 2);
        assert_eq!(bf.run_filter(&spec_one(0, "上海")).unwrap(), 1);
        assert_eq!(bf.run_filter(&spec_one(1, "abc")).unwrap(), 1);
        assert_eq!(bf.run_filter(&spec_one(1, "def")).unwrap(), 1);
        fs::remove_file(&p).ok();
    }

    /// GBK 次字节可能是 ASCII 字母：直接扫原始字节绝不能把次字节当成内容。
    #[test]
    fn big_filter_gbk_trail_byte_not_matched_as_ascii() {
        // 找一个 GBK 编码次字节为 'a' 的汉字
        let mut tricky = None;
        for cp in 0x4E00..0x9FFF {
            if let Some(ch) = char::from_u32(cp) {
                let text = ch.to_string();
                let b = encoding_rs::GBK.encode(&text).0;
                if b.len() == 2 && b[1] == b'a' {
                    tricky = Some(ch);
                    break;
                }
            }
        }
        let ch = tricky.expect("GBK 中应存在次字节为 'a' 的汉字");
        let s = format!("name,val\r\n{ch},abc\r\npln,xyz\r\n");
        let bytes = encoding_rs::GBK.encode(&s).0.into_owned();
        let p = tmp_csv("fastgbk", &bytes);
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().encoding, "gbk");
        // 该 'a' 只是双字节字符的次字节，不能命中
        assert_eq!(bf.run_filter(&spec_one(0, "a")).unwrap(), 0);
        // 非 ASCII 关键字仍能命中
        assert_eq!(bf.run_filter(&spec_one(0, &ch.to_string())).unwrap(), 1);
        // 正常 ASCII 关键字不受影响
        assert_eq!(bf.run_filter(&spec_one(1, "abc")).unwrap(), 1);
        fs::remove_file(&p).ok();
    }

    /// 列缓存：首次全扫建缓存；后续同列筛选直接走缓存；前缀收窄只扫旧命中。
    #[test]
    fn big_filter_cache_reuse_and_prefix_narrowing() {
        let p = tmp_csv("fastcache", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();

        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert!(!st.used_cache, "首次无缓存，应走全表字节扫描");
        assert_eq!(st.scanned_rows, 5);
        assert_eq!(st.cached_cols, 1, "应只缓存被筛选的列");
        assert!(st.cache_bytes > 0);

        // 同列新关键字：走缓存（语义相同）
        assert_eq!(bf.run_filter(&spec_one(2, "shang")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert!(st.used_cache, "第二次应命中列缓存");
        assert_eq!(st.scanned_rows, 5);

        // 前缀收窄：上次 "shang" 命中 2 行，本次只扫这 2 行
        assert_eq!(bf.run_filter(&spec_one(2, "shangha")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 2, "前缀收窄应只扫旧命中行");
        assert!(st.used_cache);

        // 完全相同条件：直接返回，0 行扫描
        assert_eq!(bf.run_filter(&spec_one(2, "shangha")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 0);

        // 编辑该列（Alice Beijing → Shanghai）→ 缓存失效 + 收窄基准失效，必须全扫
        bf.set_cell(0, 2, "Shanghai".into()).unwrap();
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 1);
        let st = bf.last_filter_stats();
        assert!(!st.used_cache, "编辑后该列缓存已丢，应重新全扫");
        assert_eq!(st.scanned_rows, 5, "编辑后应退回全扫，不能用旧命中收窄");

        // 撤销：值回到 Beijing，缓存再失效，重新全扫后命中恢复 2
        assert!(bf.undo());
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5);

        fs::remove_file(&p).ok();
    }

    /// 多列筛选建多列缓存；插入行不进缓存但仍参与匹配。
    #[test]
    fn big_filter_cache_multi_column_and_inserted_rows() {
        let p = tmp_csv("fastmulti", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 建 name+city 两列缓存
        let spec = FilterSpec {
            cols: vec![(0, "a".into()), (2, "beijing".into())],
            multi_cols: vec![],
            multi_text: String::new(),
        };
        assert_eq!(bf.run_filter(&spec).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().cached_cols, 2);

        // 纯多重筛选走缓存
        assert_eq!(bf.run_filter(&spec_multi(vec![0, 2], "carol")).unwrap(), 1);
        assert!(bf.last_filter_stats().used_cache);

        // 插入一行（无源记录缓存），仍须被 OR 命中
        bf.insert_rows(
            bf.meta().rows,
            Some(vec![vec!["carol2".into(), "99".into(), "Beijing".into()]]),
            1,
        )
        .unwrap();
        assert_eq!(bf.run_filter(&spec_multi(vec![0, 2], "carol")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 6);

        fs::remove_file(&p).ok();
    }

    /// 中断：代数不匹配返回 None，且不得覆盖上一次的筛选结果。
    #[test]
    fn big_filter_cancel_keeps_previous_results() {
        use std::sync::atomic::AtomicU64;
        let p = tmp_csv("fastcancel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);

        let gate = AtomicU64::new(7);
        let r = bf
            .run_filter_cancelable(&spec_one(2, "shanghai"), &gate, 6)
            .unwrap();
        assert_eq!(r, None, "代数不匹配应立即放弃");
        assert_eq!(bf.filter_len(), 2, "取消不得覆盖旧结果");
        assert_eq!(bf.filter_page(0, 10).len(), 2);

        let r = bf
            .run_filter_cancelable(&spec_one(2, "shanghai"), &gate, 7)
            .unwrap();
        assert_eq!(r, Some(2), "代数一致应正常完成");

        fs::remove_file(&p).ok();
    }

    // ============================================================
    // 性能基准（默认 --ignored；数据全部写入 %TEMP%，绝不碰用户文件）
    // ============================================================

    /// 基准环境：SIMPLE_BENCH_CSV 路径 + SIMPLE_BENCH_ROWS 行数。
    fn bench_env() -> (std::path::PathBuf, usize) {
        let path = std::env::var("SIMPLE_BENCH_CSV")
            .expect("先设置 SIMPLE_BENCH_CSV（如 %TEMP%\\big_bench.csv）");
        let rows = std::env::var("SIMPLE_BENCH_ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15_500_000);
        (std::path::PathBuf::from(path), rows)
    }

    /// 生成合成大 CSV（约 140B/行；15.5M 行 ≈ 2.2GB）。
    /// `cargo test --release --lib bench_gen_big_csv -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_gen_big_csv() {
        use std::io::Write;
        let (path, rows) = bench_env();
        let cities = [
            "北京市朝阳区", "上海市浦东新区", "广州市天河区", "深圳市南山区",
            "杭州市西湖区", "成都市武侯区", "武汉市洪山区", "西安市雁塔区",
            "南京市鼓楼区", "重庆市渝北区", "苏州市工业园区", "天津市和平区",
        ];
        let file = std::fs::File::create(&path).unwrap();
        let mut w = std::io::BufWriter::with_capacity(1 << 20, file);
        let t = std::time::Instant::now();
        writeln!(w, "id,user,city,product,price,qty,note").unwrap();
        for r in 0..rows {
            write!(
                w,
                "{:08},user-{:03},{},\"SKU-{:03} 标准商品名称\",{:>7.2},{:>2},batch-{:03} 这是一段用于填充行宽的备注描述信息ABCDEFGH\r\n",
                r,
                r % 500,
                cities[r % cities.len()],
                r % 400,
                100.0 + (r % 90_000) as f64 / 100.0,
                r % 100,
                r % 1000,
            )
            .unwrap();
        }
        w.flush().unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        eprintln!(
            "生成 {} 行 / {:.2} GB，用时 {:?}",
            rows,
            size as f64 / (1u64 << 30) as f64,
            t.elapsed()
        );
    }

    /// 打印一次筛选的耗时与引擎统计。
    fn bench_timed(bf: &mut BigFile, spec: &FilterSpec, label: &str) {
        let t = std::time::Instant::now();
        let n = bf.run_filter(spec).unwrap();
        let st = bf.last_filter_stats();
        eprintln!(
            "{label:<28} 命中 {n:>9} | 扫 {:>9} 行 | cache={} cols={} {:.0}MB | {:?}",
            st.scanned_rows,
            st.used_cache,
            st.cached_cols,
            st.cache_bytes as f64 / (1u64 << 20) as f64,
            t.elapsed()
        );
    }

    /// 大文件筛选基准：首筛建缓存 / 缓存复用 / 前缀收窄 / 中文关键字。
    /// `cargo test --release --lib bench_big_filter -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_big_filter() {
        let (path, _) = bench_env();
        let t = std::time::Instant::now();
        let mut bf = BigFile::open(&path).unwrap();
        eprintln!("open: {:?}, rows={}", t.elapsed(), bf.meta().rows);

        // 1) 首筛（无缓存，整表字节扫描 + 建 user 列缓存）
        bench_timed(&mut bf, &spec_one(1, "user-377"), "首筛 ASCII user-377");
        // 2) 同列再筛（读列缓存）
        bench_timed(&mut bf, &spec_one(1, "user-378"), "缓存复用 user-378");
        // 3) 缓存列上逐字收窄
        bench_timed(&mut bf, &spec_one(1, "user-37"), "收窄基 user-37");
        bench_timed(&mut bf, &spec_one(1, "user-377"), "收窄 user-377");
        // 4) 长备注列（单列缓存超预算 → 永久跳过建缓存）
        bench_timed(&mut bf, &spec_one(6, "batch-777"), "首筛 note batch-777");
        // 5) 前缀收窄（上一轮命中行）
        bench_timed(&mut bf, &spec_one(6, "batch-777 "), "收窄 batch-777 空格");
        // 6) 中文关键字首筛（无缓存）
        bench_timed(&mut bf, &spec_one(2, "朝阳"), "首筛 CJK 朝阳");
        // 7) 中文列缓存复用
        bench_timed(&mut bf, &spec_one(2, "浦东"), "缓存复用 CJK 浦东");
    }
}

// ============================================================
// 编码嗅探与切片解码
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Gbk,
}

/// 嗅探编码：BOM > 首部 UTF-8 合法性 > GBK 回退（对齐 read_csv_file 次序）。
/// 首部截断落在多字节字符中间时逐字节回退重试（incomplete 尾 ≠ 非法序列）。
pub fn sniff_encoding(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Encoding::Utf8;
    }
    let mut head_len = bytes.len().min(65536);
    while head_len > 0 {
        match std::str::from_utf8(&bytes[..head_len]) {
            Ok(_) => return Encoding::Utf8,
            // 尾部不完整：缩一个字节重试（截断点恰在多字节字符中间）
            Err(e) if e.error_len().is_none() => head_len -= 1,
            // 真的非法字节序列：GBK
            Err(_) => break,
        }
    }
    Encoding::Gbk
}

/// 按编码解码切片（UTF-8 合法则直转，否则有损替换；GBK 走 encoding_rs）。
fn decode_slice(enc: Encoding, bytes: &[u8]) -> String {
    match enc {
        Encoding::Utf8 => std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned()),
        Encoding::Gbk => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

// ============================================================
// 段模型：vrow 空间 → 源记录 / 插入块
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SegKind {
    /// 源记录区间 [start, start+len)。
    Src { start: u32, len: u32 },
    /// 插入块（与 blocks[block] 一一对应）。
    Ins { block: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seg {
    kind: SegKind,
    len: u32,
}

/// vrow 解析结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResolvedRow {
    Src(u32),
    Ins { block: u32, row: u32 },
}

/// 段表：维护 vrow → 源记录/插入块 的映射。
struct Segments {
    segs: Vec<Seg>,
    /// 插入块数据；与 Ins 段一一对应（None = 块已整块删除/替换）。
    blocks: Vec<Option<Vec<Vec<String>>>>,
    /// 已删除的源记录号。
    tombstones: BTreeSet<u32>,
}

impl Segments {
    fn new(n_data: u32) -> Self {
        Self {
            segs: if n_data > 0 {
                vec![Seg { kind: SegKind::Src { start: 0, len: n_data }, len: n_data }]
            } else {
                Vec::new()
            },
            blocks: Vec::new(),
            tombstones: BTreeSet::new(),
        }
    }

    fn vrow_count(&self) -> u64 {
        self.segs.iter().map(|s| s.len as u64).sum()
    }

    /// 查找包含 vrow 的段及其段内偏移。
    fn locate(&self, vrow: u64) -> Option<(usize, u32)> {
        let mut acc: u64 = 0;
        for (i, s) in self.segs.iter().enumerate() {
            if vrow < acc + s.len as u64 {
                return Some((i, (vrow - acc) as u32));
            }
            acc += s.len as u64;
        }
        None
    }

    /// 解析 vrow → 稳定身份。
    fn resolve(&self, vrow: u64) -> Option<ResolvedRow> {
        let (i, off) = self.locate(vrow)?;
        match self.segs[i].kind {
            SegKind::Src { start, .. } => Some(ResolvedRow::Src(start + off)),
            SegKind::Ins { block } => Some(ResolvedRow::Ins { block, row: off }),
        }
    }

    /// 在 vrow 处切开所在段（返回新段插入位置）；off==0 或 ==len 不切开。
    fn split_at(&mut self, vrow: u64) -> usize {
        let Some((i, off)) = self.locate(vrow) else {
            return self.segs.len();
        };
        if off == 0 {
            return i;
        }
        if off == self.segs[i].len {
            return i + 1;
        }
        let s = self.segs[i];
        match s.kind {
            SegKind::Src { start, .. } => {
                self.segs[i] = Seg { kind: SegKind::Src { start, len: off }, len: off };
                self.segs.insert(
                    i + 1,
                    Seg {
                        kind: SegKind::Src { start: start + off, len: s.len - off },
                        len: s.len - off,
                    },
                );
                i + 1
            }
            SegKind::Ins { block } => {
                // 一一对应不变式：块也切成两块，旧块作废
                let rows = self.blocks[block as usize].take().unwrap_or_default();
                let (a, b) = (rows[..off as usize].to_vec(), rows[off as usize..].to_vec());
                self.blocks.push(Some(a));
                self.blocks.push(Some(b));
                let nb1 = (self.blocks.len() - 2) as u32;
                let nb2 = (self.blocks.len() - 1) as u32;
                self.segs[i] = Seg { kind: SegKind::Ins { block: nb1 }, len: off };
                self.segs.insert(
                    i + 1,
                    Seg { kind: SegKind::Ins { block: nb2 }, len: s.len - off },
                );
                i + 1
            }
        }
    }

    /// 在 vpos 处插入一个新块（返回块号）。
    fn insert_block(&mut self, vpos: u64, rows: Vec<Vec<String>>) -> u32 {
        let at = self.split_at(vpos);
        let len = rows.len() as u32;
        self.blocks.push(Some(rows));
        let block = (self.blocks.len() - 1) as u32;
        self.segs.insert(at, Seg { kind: SegKind::Ins { block }, len });
        block
    }

    /// 删除单个 vrow（源记录进位图并分裂段；插入块行直接摘除）。
    fn delete_one(&mut self, vrow: u64) -> Option<DeletedRow> {
        let (i, off) = self.locate(vrow)?;
        let s = self.segs[i];
        match s.kind {
            SegKind::Src { start, len } => {
                let rec = start + off;
                self.tombstones.insert(rec);
                // [start,rec) + [rec+1, start+len)：被删记录从段中剔除（位图保留身份）
                self.segs[i] = Seg { kind: SegKind::Src { start, len: off }, len: off };
                self.segs.insert(
                    i + 1,
                    Seg {
                        kind: SegKind::Src { start: rec + 1, len: len - off - 1 },
                        len: len - off - 1,
                    },
                );
                Some(DeletedRow::Src { vpos: vrow, rec })
            }
            SegKind::Ins { block } => {
                let rows = self.blocks[block as usize].as_mut()?;
                let cells = rows.remove(off as usize);
                if rows.is_empty() {
                    self.blocks[block as usize] = None;
                    self.segs.remove(i);
                } else {
                    self.segs[i].len -= 1;
                }
                Some(DeletedRow::Ins { vpos: vrow, cells })
            }
        }
    }

    /// 合并相邻且源区间相接的 Src 段（撤销/恢复后收敛）。
    fn compact(&mut self) {
        let mut i = 0;
        while i + 1 < self.segs.len() {
            if let (
                SegKind::Src { start: a, len: al },
                SegKind::Src { start: b, len: bl },
            ) = (self.segs[i].kind, self.segs[i + 1].kind)
            {
                if a + al == b {
                    self.segs[i] = Seg {
                        kind: SegKind::Src { start: a, len: al + bl },
                        len: al + bl,
                    };
                    self.segs.remove(i + 1);
                    continue;
                }
            }
            i += 1;
        }
    }
}

/// 删除掉的行（撤销恢复用）。
#[derive(Clone, PartialEq, Debug)]
enum DeletedRow {
    Src { vpos: u64, rec: u32 },
    Ins { vpos: u64, cells: Vec<String> },
}

// ============================================================
// 撤销（逆操作日志）
// ============================================================

/// 单元格稳定地址。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CellId {
    Src { rec: u32, col: u32 },
    Ins { block: u32, row: u32, col: u32 },
}

/// 前向操作（重做执行它，撤销执行其逆）。
///
/// 所有寻址用稳定身份（记录号/块号），撤销/重做后压回栈的是
/// 实际执行归一化后的操作（如重做的 InsertRows 会换新块号）。
#[derive(Clone, PartialEq, Debug)]
enum Op {
    SetCell { cell: CellId, old: String, new: String },
    InsertRows { vpos: u64, block: u32, rows: Vec<Vec<String>> },
    /// items 按 vpos 升序（重做按身份重新定位）。
    RemoveRows { items: Vec<DeletedRow> },
}

/// 撤销日志上限。
const UNDO_CAP: usize = 200;
/// 单条删除操作可快照的单元格字符上限（超出则该步不可撤销）。
const UNDO_CELL_CHAR_CAP: usize = 2_000_000;

// ============================================================
// BigFile：大文件会话（mmap + 索引 + 编辑层）
// ============================================================

/// 待执行的列操作（渲染预览即时，物理重写在点击"执行"后进行）。
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ColOp {
    Insert { pos: u32, name: String },
    Delete { col: u32 },
    Rename { col: u32, name: String },
}

/// 筛选条件（与前端 grid.js _refilter 语义对齐：逐列"并且"子串 +
/// 多重筛选勾选列"或"叠加；needle/multi_text 均已小写化）。
#[derive(Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FilterSpec {
    /// (列号, 小写关键字)，列间 AND。
    pub cols: Vec<(u32, String)>,
    /// 多重筛选：勾选列 + 小写关键字（空串 = 未启用），列间 OR。
    pub multi_cols: Vec<u32>,
    pub multi_text: String,
}

/// 列投影缓存总预算（仅统计 data + offsets 常驻部分）。
const COL_CACHE_BUDGET: usize = 512 * 1024 * 1024;

/// 一次筛选的执行统计（状态栏/测试用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterStats {
    /// 实际参与匹配的行数（前缀收窄时小于总行数；完全相同条件为 0）。
    pub scanned_rows: u64,
    /// 本次是否全走列投影缓存。
    pub used_cache: bool,
    /// 当前缓存列数。
    pub cached_cols: usize,
    /// 当前缓存占用字节数。
    pub cache_bytes: usize,
}

/// 单列投影缓存：源记录值（解码后全小写 UTF-8）拼接 + 每记录起止偏移。
/// offsets 长度恒为「源记录数 + 1」，源记录号即数据记录下标（不含表头）。
struct ColCache {
    data: Vec<u8>,
    offs: Vec<u32>,
}

impl ColCache {
    #[inline]
    fn get(&self, rec: usize) -> &[u8] {
        let a = self.offs[rec] as usize;
        let b = self.offs[rec + 1] as usize;
        &self.data[a..b]
    }

    fn size(&self) -> usize {
        self.data.len() + self.offs.len() * std::mem::size_of::<u32>()
    }
}

/// 打开后的元数据。
#[derive(Clone, serde::Serialize)]
pub struct BigMeta {
    pub rows: u64,
    pub cols: u32,
    pub headers: Vec<String>,
    pub encoding: String,
    pub pending_cols: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// 后台索引是否仍在进行（true 时 rows 会持续增长）。
    pub indexing: bool,
    /// 已建立索引的字节数 / 文件总字节数。
    pub indexed_bytes: u64,
    pub total_bytes: u64,
    /// 首屏同步索引耗时（毫秒，从打开文件到返回前端）。
    pub open_ms: u64,
    /// 全量索引耗时（毫秒）；未完成时为 0。
    pub index_ms: u64,
    /// 距打开已过毫秒数（索引中实时增长）。
    pub elapsed_ms: u64,
}

/// 单条筛选关键字（已小写）。
struct Needle {
    col: u32,
    text: String,
    ascii: bool,
    multi: bool,
    hit: bool,
}

/// 扫描期间的单列缓存构建器（一个并行块一份）。
struct BuildTrack {
    col: u32,
    values: Vec<u8>,
    offs: Vec<u32>,
    first_rec: Option<u32>,
    last_rec: u32,
    failed: bool,
}

/// 并行块产出的缓存片段。
struct BuildOut {
    col: u32,
    first_rec: u32,
    last_rec: u32,
    offs: Vec<u32>,
    values: Vec<u8>,
}

/// 单列缓存合并结果。
enum MergedBuild {
    Cache(ColCache),
    /// 单列体积超过预算/4 GiB，永久放弃。
    TooBig,
    /// 片段越界（理论上不出现），本次放弃。
    Invalid,
}

/// 一次分块扫描的结果。
struct ChunkOut {
    hits: Vec<u32>,
    builds: Vec<BuildOut>,
    rows: u64,
}

/// 待扫描行来源：全量 vrow 区间，或上次命中子集。
#[derive(Clone, Copy)]
enum RowSource<'a> {
    All(u64),
    Subset(&'a [u32]),
}

impl RowSource<'_> {
    fn len(self) -> u64 {
        match self {
            RowSource::All(n) => n,
            RowSource::Subset(s) => s.len() as u64,
        }
    }

    #[inline]
    fn get(self, i: u64) -> u32 {
        match self {
            RowSource::All(_) => i as u32,
            RowSource::Subset(s) => s[i as usize],
        }
    }
}

/// 字节级字段扫描器：引号感知解析，逐字段匹配 + 顺带构建列投影缓存。
struct FieldScan<'a> {
    enc: Encoding,
    overlay: &'a HashMap<(u32, u32), String>,
    needles: Vec<Needle>,
    needle_by_col: HashMap<u32, Vec<usize>>,
    builds: Vec<BuildTrack>,
    build_by_col: HashMap<u32, usize>,
    /// 列 → 位标记：1 = 参与匹配，2 = 需要建缓存。
    col_flags: Vec<u8>,
    /// 字段原始内容（GBK 保留完整双字节对），供建缓存与非 ASCII 匹配解码用。
    field_buf: Vec<u8>,
    /// 字段的 ASCII 可匹配字节：GBK 次字节不入内，避免次字节误中 ASCII 关键字。
    match_buf: Vec<u8>,
    has_multi: bool,
}

impl<'a> FieldScan<'a> {
    /// 构造扫描器。
    ///
    /// # Parameters
    /// - `spec`: 已小写化的筛选条件。
    /// - `enc`: 文件编码（GBK 需要跳过双字节次字节）。
    /// - `overlay`: 单元格覆盖层（编辑值优先于原始字节）。
    /// - `ncols`: 物理列数。
    /// - `build_cols`: 本次需要构建缓存的物理列。
    fn new(
        spec: &FilterSpec,
        enc: Encoding,
        overlay: &'a HashMap<(u32, u32), String>,
        ncols: usize,
        build_cols: &[u32],
    ) -> Self {
        let mut needles = Vec::new();
        let mut needle_by_col: HashMap<u32, Vec<usize>> = HashMap::new();
        for (col, kw) in &spec.cols {
            let idx = needles.len();
            needles.push(Needle {
                col: *col,
                text: kw.clone(),
                ascii: kw.is_ascii(),
                multi: false,
                hit: false,
            });
            needle_by_col.entry(*col).or_default().push(idx);
        }
        if !spec.multi_text.is_empty() {
            for col in &spec.multi_cols {
                let idx = needles.len();
                needles.push(Needle {
                    col: *col,
                    text: spec.multi_text.clone(),
                    ascii: spec.multi_text.is_ascii(),
                    multi: true,
                    hit: false,
                });
                needle_by_col.entry(*col).or_default().push(idx);
            }
        }
        let mut col_flags = vec![0u8; ncols];
        for col in needle_by_col.keys() {
            if (*col as usize) < ncols {
                col_flags[*col as usize] |= 1;
            }
        }
        let mut builds = Vec::with_capacity(build_cols.len());
        let mut build_by_col = HashMap::new();
        for &col in build_cols {
            let bi = builds.len();
            builds.push(BuildTrack {
                col,
                values: Vec::new(),
                offs: Vec::new(),
                first_rec: None,
                last_rec: 0,
                failed: false,
            });
            build_by_col.insert(col, bi);
            if (col as usize) < ncols {
                col_flags[col as usize] |= 2;
            }
        }
        Self {
            enc,
            overlay,
            needles,
            needle_by_col,
            builds,
            build_by_col,
            col_flags,
            field_buf: Vec::new(),
            match_buf: Vec::new(),
            has_multi: !spec.multi_text.is_empty(),
        }
    }

    /// 源记录开始：为删除行留出的空洞补空值偏移。
    fn begin_src_record(&mut self, rec: u32) {
        for b in &mut self.builds {
            if b.failed {
                continue;
            }
            if b.first_rec.is_some() {
                // 被删除行留下的空洞：每个空洞补一个起止相同的偏移。
                while b.last_rec + 1 < rec {
                    b.offs.push(b.values.len() as u32);
                    b.last_rec += 1;
                }
            } else {
                b.first_rec = Some(rec);
                b.offs.push(b.values.len() as u32);
            }
            // 无论首行还是后续行，都必须推进到当前记录，否则后续空洞计算会多补偏移。
            b.last_rec = rec;
        }
    }

    /// 源记录结束：记录本行在缓存值中的结束偏移。
    fn finish_src_record(&mut self) {
        for b in &mut self.builds {
            if b.first_rec.is_some() && !b.failed {
                b.offs.push(b.values.len() as u32);
            }
        }
    }

    /// 字段结束：匹配该列关键字（覆盖层优先），并追加缓存值。
    fn end_field(&mut self, rec: u32, col: u32) {
        let flag = self
            .col_flags
            .get(col as usize)
            .copied()
            .unwrap_or(0);
        let overlay_val = self.overlay.get(&(rec, col));
        if flag & 1 != 0 {
            if let Some(ids) = self.needle_by_col.get(&col) {
                for &i in ids {
                    if self.needles[i].hit {
                        continue;
                    }
                    let hit = match overlay_val {
                        Some(v) => {
                            if self.needles[i].ascii {
                                v.to_ascii_lowercase()
                                    .contains(self.needles[i].text.as_str())
                            } else {
                                v.to_lowercase().contains(self.needles[i].text.as_str())
                            }
                        }
                        None if self.needles[i].ascii => memchr::memmem::find(
                            &self.match_buf,
                            self.needles[i].text.as_bytes(),
                        )
                        .is_some(),
                        None => decode_slice(self.enc, &self.field_buf)
                            .to_lowercase()
                            .contains(self.needles[i].text.as_str()),
                    };
                    self.needles[i].hit = hit;
                }
            }
        }
        if flag & 2 != 0 {
            if let Some(&bi) = self.build_by_col.get(&col) {
                match overlay_val {
                    Some(v) => append_lower_str(&mut self.builds[bi].values, v),
                    None => append_lower_bytes(
                        &mut self.builds[bi].values,
                        &self.field_buf,
                        self.enc,
                    ),
                }
                if self.builds[bi].values.len() > u32::MAX as usize {
                    self.builds[bi].failed = true;
                    self.builds[bi].values.clear();
                    self.builds[bi].offs.clear();
                }
            }
        }
        self.field_buf.clear();
        self.match_buf.clear();
    }

    /// 已扫描到第 `finalized_col` 列时，是否已可断定本行不匹配。
    /// 只在不需要建缓存（可以提前退出）时调用。
    fn definitely_fails(&self, finalized_col: u32) -> bool {
        let mut multi_hit = false;
        let mut multi_later = false;
        for n in &self.needles {
            if n.multi {
                if n.hit {
                    multi_hit = true;
                }
                if n.col > finalized_col {
                    multi_later = true;
                }
            } else if n.col <= finalized_col && !n.hit {
                return true;
            }
        }
        self.has_multi && !multi_hit && !multi_later
    }

    /// 整行结果：逐列 AND 全部命中；多重筛选启用时 OR 至少命中一列。
    fn record_result(&self) -> bool {
        let mut and_ok = true;
        let mut multi_hit = false;
        for n in &self.needles {
            if n.multi {
                multi_hit |= n.hit;
            } else if !n.hit {
                and_ok = false;
            }
        }
        and_ok && (!self.has_multi || multi_hit)
    }

    /// 扫描一条源记录（`bytes` 为原始字节区间，含结尾换行与引号内换行）。
    fn record_matches(&mut self, rec: u32, bytes: &[u8]) -> bool {
        for n in &mut self.needles {
            n.hit = false;
        }
        let n = bytes.len();
        let mut i = 0usize;
        let mut col = 0u32;
        let mut in_quotes = false;
        'fields: loop {
            let collect = (col as usize) < self.col_flags.len()
                && self.col_flags[col as usize] != 0;
            while i < n {
                let b = bytes[i];
                if in_quotes {
                    if b == b'"' {
                        if i + 1 < n && bytes[i + 1] == b'"' {
                            if collect {
                                // "" → 一个字面引号
                                self.field_buf.push(b'"');
                                self.match_buf.push(b'"');
                            }
                            i += 2;
                            continue;
                        }
                        in_quotes = false; // 闭引号不入内容
                        i += 1;
                        continue;
                    }
                    // GBK 双字节整体保留（次字节可能落在 ASCII 范围）
                    if self.enc == Encoding::Gbk && b >= 0x81 && i + 1 < n {
                        if collect {
                            // 缓存/非 ASCII 匹配需要完整双字节；ASCII 匹配只放首字节，
                            // 否则次字节可能是 'a'..'z' 造成误中。
                            self.field_buf.push(b);
                            self.field_buf.push(bytes[i + 1]);
                            self.match_buf.push(b);
                        }
                        i += 2;
                        continue;
                    }
                    if collect {
                        self.field_buf.push(b.to_ascii_lowercase());
                        self.match_buf.push(b.to_ascii_lowercase());
                    }
                    i += 1;
                    continue;
                }
                match b {
                    b'"' => {
                        in_quotes = true;
                        i += 1;
                    }
                    b',' => {
                        self.end_field(rec, col);
                        if self.builds.is_empty() && self.definitely_fails(col) {
                            return false;
                        }
                        col += 1;
                        i += 1;
                        continue 'fields; // 下一字段
                    }
                    b'\r' | b'\n' => {
                        self.end_field(rec, col);
                        return self.record_result();
                    }
                    _ => {
                        if self.enc == Encoding::Gbk && b >= 0x81 && i + 1 < n {
                            if collect {
                                self.field_buf.push(b);
                                self.field_buf.push(bytes[i + 1]);
                                self.match_buf.push(b);
                            }
                            i += 2;
                        } else {
                            if collect {
                                self.field_buf.push(b.to_ascii_lowercase());
                                self.match_buf.push(b.to_ascii_lowercase());
                            }
                            i += 1;
                        }
                    }
                }
            }
            // 末行无换行：同样按记录结束处理
            self.end_field(rec, col);
            return self.record_result();
        }
    }

    /// 取出本块构建好的缓存片段（丢弃失败的/无源行的列）。
    fn into_builds(self) -> Vec<BuildOut> {
        self.builds
            .into_iter()
            .filter_map(|b| {
                if b.failed {
                    return None;
                }
                b.first_rec.map(|first| BuildOut {
                    col: b.col,
                    first_rec: first,
                    last_rec: b.last_rec,
                    offs: b.offs,
                    values: b.values,
                })
            })
            .collect()
    }
}

/// 追加全小写 UTF-8 字节（ASCII 走字节折叠快路径）。
fn append_lower_bytes(dst: &mut Vec<u8>, raw: &[u8], enc: Encoding) {
    if raw.is_ascii() {
        dst.extend(raw.iter().map(|b| b.to_ascii_lowercase()));
        return;
    }
    let text = decode_slice(enc, raw);
    if text.is_ascii() {
        dst.extend(text.bytes().map(|b| b.to_ascii_lowercase()));
    } else {
        dst.extend_from_slice(text.to_lowercase().as_bytes());
    }
}

/// 追加字符串的全小写 UTF-8 字节。
fn append_lower_str(dst: &mut Vec<u8>, s: &str) {
    if s.is_ascii() {
        dst.extend(s.bytes().map(|b| b.to_ascii_lowercase()));
    } else {
        dst.extend_from_slice(s.to_lowercase().as_bytes());
    }
}

/// 条件涉及的物理列集合（多重筛选仅在关键字非空时参与）。
fn requested_cols(spec: &FilterSpec) -> BTreeSet<u32> {
    let mut cols: BTreeSet<u32> = spec.cols.iter().map(|(c, _)| *c).collect();
    if !spec.multi_text.is_empty() {
        cols.extend(spec.multi_cols.iter().copied());
    }
    cols
}

/// 新条件能否在旧命中集上收窄：列结构一致，且每个关键字都以前一版为前缀。
fn can_narrow(prev: &FilterSpec, next: &FilterSpec) -> bool {
    if prev.cols.len() != next.cols.len() {
        return false;
    }
    let mut pc: Vec<(u32, &str)> = prev.cols.iter().map(|(c, k)| (*c, k.as_str())).collect();
    let mut nc: Vec<(u32, &str)> = next.cols.iter().map(|(c, k)| (*c, k.as_str())).collect();
    pc.sort_by_key(|x| x.0);
    nc.sort_by_key(|x| x.0);
    for (a, b) in pc.iter().zip(nc.iter()) {
        if a.0 != b.0 || !b.1.starts_with(a.1) {
            return false;
        }
    }
    let mut pm = prev.multi_cols.clone();
    let mut nm = next.multi_cols.clone();
    pm.sort_unstable();
    nm.sort_unstable();
    if pm != nm {
        return false;
    }
    if prev.multi_text.is_empty() {
        next.multi_text.is_empty()
    } else {
        !next.multi_text.is_empty() && next.multi_text.starts_with(&prev.multi_text)
    }
}

/// 未完成的后台索引：分块扫描持续追加行偏移/记录起始，读路径短暂加锁取快照。
struct PartialIndex {
    line_off: Vec<u64>,
    rec_lines: Vec<u32>,
    cursor: ScanCursor,
    done: bool,
    /// 全量索引耗时（毫秒）；完成后写入。
    index_ms: u64,
}

/// 大文件编辑会话。
pub struct BigFile {
    path: PathBuf,
    /// 共享映射：后台索引线程持有一份，读路径不复制文件内容。
    mmap: Arc<memmap2::Mmap>,
    enc: Encoding,
    /// 已完成的全量索引；渐进索引期间为空（数据在 `partial` 里）。
    idx: RawIndex,
    /// 渐进索引状态；全量完成后由 `refresh_indexed` 并入 `idx` 并清空。
    partial: Option<Arc<Mutex<PartialIndex>>>,
    /// 会话销毁时通知后台线程退出（每块检查一次）。
    cancel: Arc<AtomicBool>,
    /// 打开起点：计算实时已用时间。
    open_started: Instant,
    /// 首屏同步索引耗时（毫秒）。
    open_ms: u64,
    /// 全量索引耗时（毫秒）；未完成时为 0。
    index_ms: u64,
    /// 文件总字节数（meta 展示进度用）。
    total_bytes: u64,
    base_ncols: usize,
    base_headers: Vec<String>,
    seg: Segments,
    /// 源记录编辑层：稳定身份 → 新值（插入行直接改块，不经此层）。
    overlay: HashMap<(u32, u32), String>,
    pending_cols: Vec<ColOp>,
    filter: Option<Vec<u32>>,
    /// 上次完成的筛选条件（前缀收窄 / 完全相同条件直接返回）。
    last_spec: Option<FilterSpec>,
    /// 列投影缓存（物理列号 → 值），LRU 受 `COL_CACHE_BUDGET` 约束。
    col_cache: HashMap<u32, ColCache>,
    cache_lru: VecDeque<u32>,
    cache_bytes: usize,
    /// 单列体积超过预算、永久放弃缓存的物理列（重载/保存后清空）。
    cache_skip: std::collections::HashSet<u32>,
    filter_stats: FilterStats,
    undo_stack: Vec<Op>,
    redo_stack: Vec<Op>,
    base_size: u64,
    base_mtime: std::time::SystemTime,
    /// 原文件是否带 UTF-8 BOM：保存/列操作重写时按原样回写（不注入也不剥除）。
    has_bom: bool,
}

impl Drop for BigFile {
    fn drop(&mut self) {
        // 后台线程每块检查；文件关闭/换档后不继续占用磁盘。
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl BigFile {
    /// 打开并建索引（秒级：三趟并行扫描，不解析字段内容）。
    ///
    /// # Errors
    /// 文件不可读/为空/无记录时返回错误字符串。
    pub fn open(path: &Path) -> Result<Self, String> {
        let t0 = Instant::now();
        let file = fs::File::open(path)
            .map_err(|e| format!("打开失败：{}（{}）", path.display(), e))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.len() == 0 {
            return Err("空文件".into());
        }
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        let enc = sniff_encoding(bytes);
        // BOM 只对 UTF-8 有意义（GBK 文件可能以相同字节开头纯属巧合）
        let has_bom = enc == Encoding::Utf8 && bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let idx = build_index(bytes);
        if idx.n_records() == 0 {
            return Err("文件中没有记录".into());
        }
        let (hs, he) = idx.record_span(bytes.len(), 0);
        let mut headers = parse_record(&decode_slice(enc, &bytes[hs..he]), b',');
        if enc == Encoding::Utf8 {
            if let Some(first) = headers.first_mut() {
                if let Some(stripped) = first.strip_prefix('\u{FEFF}') {
                    *first = stripped.to_string();
                }
            }
        }
        let n_data = idx.n_records() - 1;
        let base_ncols = headers.len();
        let open_ms = t0.elapsed().as_millis() as u64;
        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            enc,
            idx,
            partial: None,
            cancel: Arc::new(AtomicBool::new(false)),
            open_started: t0,
            open_ms,
            index_ms: open_ms,
            total_bytes: meta.len(),
            base_ncols,
            base_headers: headers,
            seg: Segments::new(n_data as u32),
            overlay: HashMap::new(),
            pending_cols: Vec::new(),
            filter: None,
            last_spec: None,
            col_cache: HashMap::new(),
            cache_lru: VecDeque::new(),
            cache_bytes: 0,
            cache_skip: std::collections::HashSet::new(),
            filter_stats: FilterStats::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            base_size: meta.len(),
            base_mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            has_bom,
        })
    }

    /// 渐进打开：同步扫一小段前缀让表格立即可用，剩余部分后台继续建索引。
    ///
    /// # Parameters
    /// - `path`: CSV 文件路径。
    ///
    /// # Errors
    /// 文件不可读/为空/首条记录无法完成时返回错误字符串。
    pub fn open_progressive(path: &Path) -> Result<Self, String> {
        Self::open_progressive_with(
            path,
            INITIAL_INDEX_BYTES,
            INITIAL_INDEX_RECORDS,
            INDEX_CHUNK_BYTES,
            true,
        )
    }

    /// 渐进打开的可调参版本（测试用小阈值/同步驱动）。
    ///
    /// # Parameters
    /// - `path`: CSV 文件路径。
    /// - `initial_bytes`: 同步索引最多推进字节数。
    /// - `initial_records`: 同步索引最多记录数（含表头）。
    /// - `chunk_bytes`: 后台每块推进字节数。
    /// - `background`: true = 启动后台线程；false = 保持部分索引（测试手动驱动）。
    fn open_progressive_with(
        path: &Path,
        initial_bytes: usize,
        initial_records: usize,
        chunk_bytes: usize,
        background: bool,
    ) -> Result<Self, String> {
        let t0 = Instant::now();
        let file = fs::File::open(path)
            .map_err(|e| format!("打开失败：{}（{}）", path.display(), e))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.len() == 0 {
            return Err("空文件".into());
        }
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        let enc = sniff_encoding(bytes);
        let has_bom = enc == Encoding::Utf8 && bytes.starts_with(&[0xEF, 0xBB, 0xBF]);

        // ---- 首屏同步前缀：至少一条完整记录（表头），再按字节/记录数收手 ----
        let mut cur = ScanCursor::new();
        let mut line_off = vec![0u64];
        let mut rec_lines: Vec<u32> = Vec::new();
        let mut done = false;
        loop {
            let (o, r) = scan_chunk(bytes, &mut cur, initial_bytes.max(1));
            line_off.extend(o);
            rec_lines.extend(r);
            if cur.pos >= bytes.len() {
                done = true;
                break;
            }
            let usable =
                usable_record_count(rec_lines.len(), cur.line_started_open, false);
            if usable >= 1
                && (cur.pos >= initial_bytes
                    || usable >= initial_records.saturating_add(1))
            {
                break;
            }
        }
        let usable = usable_record_count(rec_lines.len(), cur.line_started_open, done);
        if usable == 0 {
            return Err("文件中没有记录".into());
        }
        // 表头记录必定完整；parse_record 在首个引号外换行处停止，后缀内容
        // （后续记录）不会混入表头。
        let header_start = line_off[rec_lines[0] as usize] as usize;
        let header_end = if done { bytes.len() } else { cur.pos };
        let mut headers =
            parse_record(&decode_slice(enc, &bytes[header_start..header_end]), b',');
        if enc == Encoding::Utf8 {
            if let Some(first) = headers.first_mut() {
                if let Some(stripped) = first.strip_prefix('\u{FEFF}') {
                    *first = stripped.to_string();
                }
            }
        }
        let base_ncols = headers.len();
        let n_data = usable - 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let open_ms = t0.elapsed().as_millis() as u64;
        let total_bytes = meta.len();

        let (partial, idx, index_ms) = if done {
            (None, RawIndex { line_off, rec_lines }, open_ms)
        } else {
            let p = Arc::new(Mutex::new(PartialIndex {
                line_off,
                rec_lines,
                cursor: cur,
                done: false,
                index_ms: 0,
            }));
            if background {
                let mmap2 = mmap.clone();
                let p2 = p.clone();
                let c2 = cancel.clone();
                std::thread::Builder::new()
                    .name("csv-index".into())
                    .spawn(move || {
                        Self::index_worker(mmap2, p2, c2, cur, chunk_bytes, t0);
                    })
                    .map_err(|e| format!("启动索引线程失败：{e}"))?;
            }
            (
                Some(p),
                RawIndex { line_off: Vec::new(), rec_lines: Vec::new() },
                0,
            )
        };

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            enc,
            idx,
            partial,
            cancel,
            open_started: t0,
            open_ms,
            index_ms,
            total_bytes,
            base_ncols,
            base_headers: headers,
            seg: Segments::new(n_data as u32),
            overlay: HashMap::new(),
            pending_cols: Vec::new(),
            filter: None,
            last_spec: None,
            col_cache: HashMap::new(),
            cache_lru: VecDeque::new(),
            cache_bytes: 0,
            cache_skip: std::collections::HashSet::new(),
            filter_stats: FilterStats::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            base_size: meta.len(),
            base_mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            has_bom,
        })
    }

    /// 后台索引线程主体：单线程顺序扫描剩余字节，每块发布一次。
    ///
    /// # Parameters
    /// - `mmap`: 文件映射（与会话共享）。
    /// - `partial`: 发布区（读路径短暂加锁）。
    /// - `cancel`: 会话销毁标志。
    /// - `cur`: 从前缀扫描继续的游标。
    /// - `chunk_bytes`: 每块推进字节数。
    /// - `started`: 打开起点（全量索引耗时从这里算）。
    fn index_worker(
        mmap: Arc<memmap2::Mmap>,
        partial: Arc<Mutex<PartialIndex>>,
        cancel: Arc<AtomicBool>,
        mut cur: ScanCursor,
        chunk_bytes: usize,
        started: Instant,
    ) {
        // 磁盘顺序扫描是重 IO：线程级降到 BELOW_NORMAL，让 UI/读页优先。
        let _bg = bg::BackgroundGuard::enter();
        let bytes: &[u8] = &mmap;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let (o, r) = scan_chunk(bytes, &mut cur, chunk_bytes.max(1));
            let done = cur.pos >= bytes.len();
            let mut st = partial.lock().unwrap();
            st.line_off.extend(o);
            st.rec_lines.extend(r);
            st.cursor = cur;
            if done {
                st.done = true;
                st.index_ms = started.elapsed().as_millis() as u64;
                return;
            }
            drop(st); // 每块之间放锁：读路径可随时取已扫部分
        }
    }

    /// 测试辅助：同步扫完剩余部分（不经过后台线程）。
    #[cfg(test)]
    fn drive_index_for_test(&mut self) {
        let Some(p) = self.partial.clone() else { return };
        let mut cur = p.lock().unwrap().cursor;
        let bytes: &[u8] = &self.mmap;
        loop {
            let (o, r) = scan_chunk(bytes, &mut cur, INDEX_CHUNK_BYTES);
            let done = cur.pos >= bytes.len();
            let mut st = p.lock().unwrap();
            st.line_off.extend(o);
            st.rec_lines.extend(r);
            st.cursor = cur;
            if done {
                st.done = true;
                st.index_ms = self.open_started.elapsed().as_millis() as u64;
                return;
            }
            drop(st);
        }
    }

    /// 后台索引是否仍在进行。
    pub fn is_indexing(&self) -> bool {
        self.partial
            .as_ref()
            .is_some_and(|p| !p.lock().unwrap().done)
    }

    /// 全量索引完成后并回正式结构；索引期仅允许单元格编辑，故段表可安全重建
    /// （覆盖层按稳定记录号寻址，原样保留）。
    pub fn refresh_indexed(&mut self) {
        let Some(p) = self.partial.clone() else { return };
        let done = p.lock().unwrap().done;
        if !done {
            return;
        }
        let (line_off, rec_lines, index_ms) = {
            let mut st = p.lock().unwrap();
            (
                std::mem::take(&mut st.line_off),
                std::mem::take(&mut st.rec_lines),
                st.index_ms,
            )
        };
        let n_data = rec_lines.len().saturating_sub(1);
        self.idx = RawIndex { line_off, rec_lines };
        self.seg = Segments::new(n_data as u32);
        self.index_ms = index_ms;
        self.partial = None;
    }

    /// 当前可展示的数据行数（渐进索引期间随扫描增长）。
    fn indexed_data_rows(&self) -> u64 {
        if let Some(p) = &self.partial {
            let st = p.lock().unwrap();
            usable_record_count(
                st.rec_lines.len(),
                st.cursor.line_started_open,
                st.done,
            )
            .saturating_sub(1) as u64
        } else {
            self.seg.vrow_count()
        }
    }

    /// 记录 r（含表头，0 基）的字节范围；渐进索引期间只暴露完整记录。
    fn record_span_visible(&self, r: usize) -> Option<(usize, usize)> {
        if let Some(p) = &self.partial {
            let st = p.lock().unwrap();
            let usable = usable_record_count(
                st.rec_lines.len(),
                st.cursor.line_started_open,
                st.done,
            );
            if r >= usable {
                return None;
            }
            let start = st.line_off[st.rec_lines[r] as usize] as usize;
            let end = if r + 1 < st.rec_lines.len() {
                st.line_off[st.rec_lines[r + 1] as usize] as usize
            } else {
                st.cursor.pos
            };
            Some((start, end.min(self.mmap.len())))
        } else {
            if r >= self.idx.n_records() {
                return None;
            }
            Some(self.idx.record_span(self.mmap.len(), r))
        }
    }

    pub fn meta(&self) -> BigMeta {
        let (rows, indexing, indexed_bytes, index_ms, elapsed_ms) =
            if let Some(p) = &self.partial {
                let st = p.lock().unwrap();
                (
                    usable_record_count(
                        st.rec_lines.len(),
                        st.cursor.line_started_open,
                        st.done,
                    )
                    .saturating_sub(1) as u64,
                    !st.done,
                    st.cursor.pos as u64,
                    st.index_ms,
                    self.open_started.elapsed().as_millis() as u64,
                )
            } else {
                (
                    self.seg.vrow_count(),
                    false,
                    self.total_bytes,
                    self.index_ms,
                    self.index_ms,
                )
            };
        BigMeta {
            rows,
            cols: self.view_ncols(),
            headers: self.view_headers(),
            encoding: match self.enc {
                Encoding::Utf8 => "utf-8".into(),
                Encoding::Gbk => "gbk".into(),
            },
            pending_cols: !self.pending_cols.is_empty(),
            can_undo: !self.undo_stack.is_empty(),
            can_redo: !self.redo_stack.is_empty(),
            indexing,
            indexed_bytes,
            total_bytes: self.total_bytes,
            open_ms: self.open_ms,
            index_ms,
            elapsed_ms,
        }
    }

    /// 外部改动探测（窗口聚焦时前端轮询）。
    pub fn stale(&self) -> bool {
        let Ok(m) = fs::metadata(&self.path) else { return true };
        m.len() != self.base_size || m.modified().ok() != Some(self.base_mtime)
    }

    // ---- 列视图（pending 映射） ----

    /// 映射后列数 = 基准列 + 插入 − 删除。
    fn view_ncols(&self) -> u32 {
        let mut n = self.base_ncols as i64;
        for op in &self.pending_cols {
            match op {
                ColOp::Insert { .. } => n += 1,
                ColOp::Delete { .. } => n -= 1,
                ColOp::Rename { .. } => {}
            }
        }
        n.max(0) as u32
    }

    /// 映射后表头。
    fn view_headers(&self) -> Vec<String> {
        let mut cols: Vec<Option<String>> =
            self.base_headers.iter().cloned().map(Some).collect();
        for op in &self.pending_cols {
            match op {
                ColOp::Insert { pos, name } => {
                    cols.insert((*pos as usize).min(cols.len()), Some(name.clone()));
                }
                ColOp::Delete { col } => {
                    if (*col as usize) < cols.len() {
                        cols.remove(*col as usize);
                    }
                }
                ColOp::Rename { col, name } => {
                    if let Some(Some(h)) = cols.get_mut(*col as usize) {
                        *h = name.clone();
                    }
                }
            }
        }
        cols.into_iter().map(|c| c.unwrap_or_default()).collect()
    }

    /// 视图列 → 物理列（None = 待插入的虚拟列）。
    fn view_to_phys(&self, vc: u32) -> Option<u32> {
        let mut phys: Vec<Option<u32>> = (0..self.base_ncols as u32).map(Some).collect();
        for op in &self.pending_cols {
            match op {
                ColOp::Insert { pos, .. } => {
                    phys.insert((*pos as usize).min(phys.len()), None);
                }
                ColOp::Delete { col } => {
                    if (*col as usize) < phys.len() {
                        phys.remove(*col as usize);
                    }
                }
                ColOp::Rename { .. } => {}
            }
        }
        phys.get(vc as usize).copied().flatten()
    }

    /// 物理字段数组 → 视图字段数组（pending 期读路径）。
    fn map_fields_to_view(&self, fields: &[String]) -> Vec<String> {
        if self.pending_cols.is_empty() {
            return fields.to_vec();
        }
        let n = self.view_ncols() as usize;
        let mut out = vec![String::new(); n];
        for vc in 0..n {
            if let Some(p) = self.view_to_phys(vc as u32) {
                if let Some(f) = fields.get(p as usize) {
                    out[vc] = f.clone();
                }
            }
        }
        out
    }

    fn require_editable(&self) -> Result<(), String> {
        if !self.pending_cols.is_empty() {
            return Err("列操作待执行：请先执行或撤销列操作".into());
        }
        Ok(())
    }

    // ---- 读路径 ----

    /// 读取源记录字段（解码 + 解析 + pad/truncate + 覆盖层）。
    /// 把源文件 data rec 区间 [first, end) 的原始字节整段写入（透传不重编码）。
    /// 端点取记录首行偏移；区间末尾即文件末时用 mmap 长度。
    /// 仅在 save() 透传路径使用：调用方保证行未删除（段已剔除被删行）、无覆盖层。
    fn write_raw_range(&self, w: &mut impl Write, first: u32, end: u32) -> Result<(), String> {
        let s = self.idx.record_span(self.mmap.len(), first as usize + 1).0;
        let e = if (end as usize + 1) < self.idx.n_records() {
            self.idx.record_span(self.mmap.len(), end as usize + 1).0
        } else {
            self.mmap.len()
        };
        w.write_all(&self.mmap[s..e]).map_err(|e| e.to_string())
    }

    fn src_fields(&self, rec: u32) -> Vec<String> {
        // 渐进索引期间只允许读已完整发布的记录；未命中给空行（调用方已按
        // 可见行数过滤，正常不会发生）。
        let Some((s, e)) = self.record_span_visible(rec as usize + 1) else {
            return vec![String::new(); self.base_ncols];
        };
        let mut f = parse_record(&decode_slice(self.enc, &self.mmap[s..e]), b',');
        let n = self.base_ncols;
        if f.len() < n {
            f.resize(n, String::new());
        } else if f.len() > n {
            f.truncate(n);
        }
        for c in 0..n {
            if let Some(v) = self.overlay.get(&(rec, c as u32)) {
                f[c].clone_from(v);
            }
        }
        f
    }

    /// 读取插入行字段（pad/truncate；无覆盖层，编辑直改块）。
    fn ins_fields(&self, block: u32, row: u32) -> Vec<String> {
        let n = self.base_ncols;
        let mut f = self
            .seg
            .blocks
            .get(block as usize)
            .and_then(|b| b.as_ref())
            .and_then(|rows| rows.get(row as usize))
            .cloned()
            .unwrap_or_default();
        if f.len() < n {
            f.resize(n, String::new());
        } else if f.len() > n {
            f.truncate(n);
        }
        f
    }

    /// 取 vrow 的视图行（pending 期做列映射；渐进索引期 vrow == 源记录号）。
    fn row_at(&self, vrow: u64) -> Option<Vec<String>> {
        if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return None;
            }
            return Some(self.map_fields_to_view(&self.src_fields(vrow as u32)));
        }
        match self.seg.resolve(vrow)? {
            ResolvedRow::Src(rec) => {
                let f = self.src_fields(rec);
                Some(self.map_fields_to_view(&f))
            }
            ResolvedRow::Ins { block, row } => {
                let f = self.ins_fields(block, row);
                Some(self.map_fields_to_view(&f))
            }
        }
    }

    /// 取行窗口 [from, from+count)。
    pub fn rows(&self, from: u64, count: u32) -> Vec<Vec<String>> {
        let total = if self.partial.is_some() {
            self.indexed_data_rows()
        } else {
            self.seg.vrow_count()
        };
        (from..(from + count as u64).min(total))
            .filter_map(|v| self.row_at(v))
            .collect()
    }

    // ---- 编辑 ----

    /// 读取单元格当前值（编辑前旧值用）。
    fn cell_value(&self, cell: CellId) -> String {
        match cell {
            CellId::Src { rec, col } => {
                if let Some(v) = self.overlay.get(&(rec, col)) {
                    return v.clone();
                }
                self.src_fields(rec).get(col as usize).cloned().unwrap_or_default()
            }
            CellId::Ins { block, row, col } => self
                .seg
                .blocks
                .get(block as usize)
                .and_then(|b| b.as_ref())
                .and_then(|rows| rows.get(row as usize))
                .and_then(|cells| cells.get(col as usize))
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// 写单元格（覆盖层/直改块 + 撤销日志）。
    pub fn set_cell(&mut self, vrow: u64, col: u32, value: String) -> Result<(), String> {
        self.require_editable()?;
        // 渐进索引期没有插入/删除行，vrow 恒等于源记录号；完成后再走段表。
        let resolved = if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return Err("行号越界".into());
            }
            ResolvedRow::Src(vrow as u32)
        } else {
            self.seg.resolve(vrow).ok_or("行号越界")?
        };
        let cell = match resolved {
            ResolvedRow::Src(rec) => CellId::Src { rec, col },
            ResolvedRow::Ins { block, row } => CellId::Ins { block, row, col },
        };
        let old = self.cell_value(cell);
        if old == value {
            return Ok(());
        }
        // 收窄基准必然失效；源值变化还要丢掉该列投影缓存
        self.last_spec = None;
        if matches!(cell, CellId::Src { .. }) {
            self.cache_drop(col);
        }
        match cell {
            CellId::Src { rec, col } => {
                self.overlay.insert((rec, col), value.clone());
            }
            CellId::Ins { block, row, col } => {
                if let Some(Some(rows)) = self.seg.blocks.get_mut(block as usize) {
                    if let Some(cells) = rows.get_mut(row as usize) {
                        if (col as usize) >= cells.len() {
                            cells.resize(col as usize + 1, String::new());
                        }
                        cells[col as usize] = value.clone();
                    }
                }
            }
        }
        self.push_undo(Op::SetCell { cell, old, new: value });
        Ok(())
    }

    /// 在 vpos 处插入行（rows 为空串行时传 None + count）。
    pub fn insert_rows(
        &mut self,
        vpos: u64,
        rows: Option<Vec<Vec<String>>>,
        count: u32,
    ) -> Result<(), String> {
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持插入/删除行".into());
        }
        if vpos > self.seg.vrow_count() {
            return Err("插入位置越界".into());
        }
        let rows = rows.unwrap_or_else(|| {
            vec![vec![String::new(); self.base_ncols]; count as usize]
        });
        if rows.is_empty() {
            return Ok(());
        }
        let ncols = self.base_ncols;
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|mut r| {
                if r.len() < ncols {
                    r.resize(ncols, String::new());
                } else if r.len() > ncols {
                    r.truncate(ncols);
                }
                r
            })
            .collect();
        let len = rows.len() as u64;
        let block = self.seg.insert_block(vpos, rows.clone());
        self.push_undo(Op::InsertRows { vpos, block, rows });
        self.last_spec = None; // vrow 结构变化：旧命中行号不可再用于收窄
        let _ = len;
        Ok(())
    }

    /// 删除一组 vrow（升序传入；快照超限的删除不可撤销）。
    pub fn remove_rows(&mut self, mut vrows: Vec<u64>) -> Result<u32, String> {
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持插入/删除行".into());
        }
        vrows.sort_unstable();
        vrows.dedup();
        let total = self.seg.vrow_count();
        if vrows.iter().any(|&v| v >= total) {
            return Err("行号越界".into());
        }
        // 降序删除，保证低 vrow 位置稳定
        let mut items: Vec<DeletedRow> = Vec::new();
        let mut chars: usize = 0;
        let mut undoable = true;
        for &v in vrows.iter().rev() {
            match self.seg.delete_one(v) {
                Some(DeletedRow::Src { vpos, rec }) => {
                    chars += self.base_ncols * 8;
                    items.push(DeletedRow::Src { vpos, rec });
                }
                Some(DeletedRow::Ins { vpos, cells }) => {
                    chars += cells.iter().map(|c| c.len()).sum::<usize>() + cells.len();
                    items.push(DeletedRow::Ins { vpos, cells });
                }
                None => undoable = false,
            }
            if chars > UNDO_CELL_CHAR_CAP {
                undoable = false;
            }
        }
        items.reverse(); // 恢复按升序
        let n = items.len() as u32;
        if undoable {
            self.push_undo(Op::RemoveRows { items });
        } else {
            self.undo_stack.clear();
            self.redo_stack.clear();
        }
        self.last_spec = None; // vrow 结构变化：旧命中行号不可再用于收窄
        Ok(n)
    }

    fn push_undo(&mut self, op: Op) {
        self.undo_stack.push(op);
        if self.undo_stack.len() > UNDO_CAP {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    fn apply(&mut self, op: &Op) -> Op {
        match op.clone() {
            Op::SetCell { cell, old: _, new } => {
                self.write_cell(cell, &new);
                op.clone()
            }
            Op::InsertRows { vpos, block: _, rows } => {
                let block = self.seg.insert_block(vpos, rows.clone());
                Op::InsertRows { vpos, block, rows }
            }
            Op::RemoveRows { items } => {
                // 重做：撤销已把行放回原始 vpos，按 vpos 降序重新删除
                //（先删高 vrow，低 vrow 位置不受影响；源行与插入行同规则）
                let mut order: Vec<&DeletedRow> = items.iter().collect();
                order.sort_by_key(|d| match d {
                    DeletedRow::Src { vpos, .. } => *vpos,
                    DeletedRow::Ins { vpos, .. } => *vpos,
                });
                let mut deleted: Vec<DeletedRow> = Vec::new();
                for it in order.iter().rev() {
                    let vpos = match it {
                        DeletedRow::Src { vpos, .. } => *vpos,
                        DeletedRow::Ins { vpos, .. } => *vpos,
                    };
                    if let Some(d) = self.seg.delete_one(vpos) {
                        deleted.push(d);
                    }
                }
                deleted.sort_by_key(|d| match d {
                    DeletedRow::Src { vpos, .. } => *vpos,
                    DeletedRow::Ins { vpos, .. } => *vpos,
                });
                Op::RemoveRows { items: deleted }
            }
        }
    }

    fn write_cell(&mut self, cell: CellId, value: &str) {
        match cell {
            CellId::Src { rec, col } => {
                self.overlay.insert((rec, col), value.to_string());
            }
            CellId::Ins { block, row, col } => {
                if let Some(Some(rows)) = self.seg.blocks.get_mut(block as usize) {
                    if let Some(cells) = rows.get_mut(row as usize) {
                        if (col as usize) >= cells.len() {
                            cells.resize(col as usize + 1, String::new());
                        }
                        cells[col as usize] = value.to_string();
                    }
                }
            }
        }
    }

    /// 源记录当前 vrow（重做删除时重新定位；不存在 = 已删）。

    /// 撤销。
    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo_stack.pop() else { return false };
        // 覆盖层/行结构都可能回退：缓存与收窄基准一律作废
        self.cache_clear_all();
        self.last_spec = None;
        let restored = self.apply_inverse(&op);
        self.redo_stack.push(restored);
        true
    }

    /// 重做。
    pub fn redo(&mut self) -> bool {
        let Some(op) = self.redo_stack.pop() else { return false };
        self.cache_clear_all();
        self.last_spec = None;
        let normalized = self.apply(&op);
        self.undo_stack.push(normalized);
        true
    }

    fn apply_inverse(&mut self, op: &Op) -> Op {
        match op.clone() {
            Op::SetCell { cell, old, .. } => {
                self.write_cell(cell, &old);
                op.clone() // 重做时 apply 会写回 new
            }
            Op::InsertRows { vpos, rows, block: _ } => {
                // 逆 = 摘除位于 vpos 的那行块。删除+恢复会把块换成新 id，
                // 故按 位置+内容 定位，不依赖 block id（重做时 apply 重新插块）
                if let Some((i, off)) = self.seg.locate(vpos) {
                    if let SegKind::Ins { block } = self.seg.segs[i].kind {
                        let same = off == 0
                            && self.seg.segs[i].len == rows.len() as u32
                            && self
                                .seg
                                .blocks
                                .get(block as usize)
                                .and_then(|b| b.as_ref())
                                .is_some_and(|r| *r == rows);
                        if same {
                            self.seg.segs.remove(i);
                            self.seg.blocks[block as usize] = None;
                        }
                    }
                }
                op.clone()
            }
            Op::RemoveRows { items } => {
                // 逆 = 按 vpos 升序逐行放回（低 vrow 先复位，高 vrow 的
                // 原始位置随之恢复；降序放回会让高 vrow 插错位置）
                let mut restored: Vec<DeletedRow> = Vec::new();
                for it in items.iter() {
                    match it {
                        DeletedRow::Src { vpos, rec } => {
                            self.seg.tombstones.remove(rec);
                            let at = self.seg.split_at(*vpos);
                            self.seg.segs.insert(
                                at,
                                Seg {
                                    kind: SegKind::Src { start: *rec, len: 1 },
                                    len: 1,
                                },
                            );
                            restored.push(it.clone());
                        }
                        DeletedRow::Ins { .. } => {
                            // 放回为新块（重做时按策略跳过插入行）
                            if let DeletedRow::Ins { vpos, cells } = it {
                                self.seg.insert_block(*vpos, vec![cells.clone()]);
                                restored.push(it.clone());
                            }
                        }
                    }
                }
                self.seg.compact();
                Op::RemoveRows { items: restored }
            }
        }
    }

    // ---- 段累计长度表（O(log segs) 解析，筛选热路径用） ----

    fn seg_cum_lens(&self) -> Vec<u64> {
        let mut acc = 0u64;
        self.seg
            .segs
            .iter()
            .map(|s| {
                acc += s.len as u64;
                acc
            })
            .collect()
    }

    fn resolve_fast(&self, cum: &[u64], vrow: u64) -> Option<ResolvedRow> {
        let i = cum.partition_point(|&c| c <= vrow);
        let seg = self.seg.segs.get(i)?;
        let base = if i == 0 { 0 } else { cum[i - 1] };
        let off = (vrow - base) as u32;
        match seg.kind {
            SegKind::Src { start, .. } => Some(ResolvedRow::Src(start + off)),
            SegKind::Ins { block } => Some(ResolvedRow::Ins { block, row: off }),
        }
    }

    fn row_at_fast(&self, cum: &[u64], vrow: u64) -> Option<Vec<String>> {
        match self.resolve_fast(cum, vrow)? {
            ResolvedRow::Src(rec) => {
                let f = self.src_fields(rec);
                Some(self.map_fields_to_view(&f))
            }
            ResolvedRow::Ins { block, row } => {
                let f = self.ins_fields(block, row);
                Some(self.map_fields_to_view(&f))
            }
        }
    }

    // ---- 筛选 ----

    /// 执行筛选（无中断门），命中列表存引擎侧，返回命中数。
    ///
    /// # Errors
    /// 列操作待执行 / 行数超出 u32 命中集上限时返回错误字符串。
    #[cfg(test)]
    pub fn run_filter(&mut self, spec: &FilterSpec) -> Result<u32, String> {
        match self.run_filter_inner(spec, None)? {
            Some(n) => Ok(n),
            // 未传中断门时不会取消；分支仅为类型完整
            None => Ok(0),
        }
    }

    /// 可中断筛选：扫描期间 `cancel` 的值偏离 `gen` 时放弃并返回 `Ok(None)`，
    /// 上一次筛选结果原样保留。
    ///
    /// # Parameters
    /// - `spec`: 筛选条件（关键字大小写不敏感）。
    /// - `cancel`: 共享代数计数器；新请求自增即可让旧扫描退出。
    /// - `gen`: 本次请求的代数快照。
    pub fn run_filter_cancelable(
        &mut self,
        spec: &FilterSpec,
        cancel: &AtomicU64,
        gen: u64,
    ) -> Result<Option<u32>, String> {
        self.run_filter_inner(spec, Some((cancel, gen)))
    }

    /// 最近一次筛选的执行统计。
    #[cfg(test)]
    pub fn last_filter_stats(&self) -> FilterStats {
        self.filter_stats
    }

    fn run_filter_inner(
        &mut self,
        spec: &FilterSpec,
        gate: Option<(&AtomicU64, u64)>,
    ) -> Result<Option<u32>, String> {
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持筛选".into());
        }
        let total = self.seg.vrow_count();
        if total > u32::MAX as u64 {
            return Err("文件过大（超过 42.9 亿行），暂不支持筛选".into());
        }
        let cancelled = || gate.is_some_and(|(g, id)| g.load(Ordering::Relaxed) != id);
        if cancelled() {
            return Ok(None);
        }

        // 契约：needle 已小写；再归一化一次防守（成本 O(条件数)）
        let mut spec = spec.clone();
        for (_, kw) in &mut spec.cols {
            *kw = kw.to_lowercase();
        }
        spec.multi_text = spec.multi_text.to_lowercase();

        // 完全相同条件：直接复用上次结果（编辑/插删/撤销都会清空 last_spec）
        if self.filter.is_some() && self.last_spec.as_ref() == Some(&spec) {
            self.filter_stats = FilterStats {
                scanned_rows: 0,
                used_cache: self.cache_covers(&spec),
                cached_cols: self.col_cache.len(),
                cache_bytes: self.cache_bytes,
            };
            return Ok(Some(self.filter_len()));
        }

        // 无条件 = 全命中（不扫字节）
        if spec.cols.is_empty() && spec.multi_text.is_empty() {
            let all: Vec<u32> = (0..total as u32).collect();
            self.filter = Some(all);
            self.last_spec = Some(spec);
            self.filter_stats = FilterStats {
                scanned_rows: total,
                used_cache: false,
                cached_cols: self.col_cache.len(),
                cache_bytes: self.cache_bytes,
            };
            return Ok(Some(total as u32));
        }

        // 多重筛选启用却未勾选任何列：任何行都不可能命中
        if !spec.multi_text.is_empty() && spec.multi_cols.is_empty() {
            self.filter = Some(Vec::new());
            self.last_spec = Some(spec);
            self.filter_stats = FilterStats {
                scanned_rows: 0,
                used_cache: false,
                cached_cols: self.col_cache.len(),
                cache_bytes: self.cache_bytes,
            };
            return Ok(Some(0));
        }

        let use_cache = self.cache_covers(&spec);

        // 前缀收窄：同列结构 + 关键字逐字追加 → 只扫上次命中行
        let narrow = self
            .last_spec
            .as_ref()
            .filter(|prev| can_narrow(prev, &spec))
            .and_then(|_| self.filter.take());
        let narrowing = narrow.is_some();

        // 缺缓存且本次是全扫：顺带建缓存（收窄只有命中行，建不出全列缓存）
        let build_cols: Vec<u32> = if narrowing || use_cache {
            Vec::new()
        } else {
            requested_cols(&spec)
                .into_iter()
                .filter(|&c| (c as usize) < self.base_ncols)
                .filter(|c| {
                    !self.col_cache.contains_key(c) && !self.cache_skip.contains(c)
                })
                .collect()
        };

        let source = match narrow.as_deref() {
            Some(rows) => RowSource::Subset(rows),
            None => RowSource::All(total),
        };
        let chunk = 1u64 << 16;
        let nchunks = source.len().div_ceil(chunk).max(1) as usize;
        let parts: Vec<Option<ChunkOut>> = (0..nchunks)
            .into_par_iter()
            .map(|ci| {
                let start = ci as u64 * chunk;
                let end = (start + chunk).min(source.len());
                self.scan_chunk(&spec, use_cache, &build_cols, gate, source, start, end)
            })
            .collect();

        // 任一缺口被取消：恢复旧命中集，绝不覆盖
        if parts.iter().any(|p| p.is_none()) {
            if let Some(prev) = narrow {
                self.filter = Some(prev);
            }
            return Ok(None);
        }

        let mut all: Vec<u32> = Vec::new();
        let mut builds: HashMap<u32, Vec<BuildOut>> = HashMap::new();
        let mut scanned = 0u64;
        for part in parts.into_iter().flatten() {
            scanned += part.rows;
            all.extend(part.hits);
            for b in part.builds {
                builds.entry(b.col).or_default().push(b);
            }
        }

        // 合并块内片段 → 装入列缓存（LRU/预算由 cache_put 负责）
        for (col, chunks) in builds {
            match self.merge_builds(&chunks) {
                MergedBuild::Cache(cache) => self.cache_put(col, cache),
                MergedBuild::TooBig => {
                    self.cache_skip.insert(col);
                }
                MergedBuild::Invalid => {}
            }
        }

        let n = all.len() as u32;
        self.filter = Some(all);
        self.last_spec = Some(spec);
        self.filter_stats = FilterStats {
            scanned_rows: scanned,
            used_cache: use_cache,
            cached_cols: self.col_cache.len(),
            cache_bytes: self.cache_bytes,
        };
        Ok(Some(n))
    }

    /// 扫描一个并行块（全量或收窄子集）。
    fn scan_chunk(
        &self,
        spec: &FilterSpec,
        use_cache: bool,
        build_cols: &[u32],
        gate: Option<(&AtomicU64, u64)>,
        source: RowSource,
        start: u64,
        end: u64,
    ) -> Option<ChunkOut> {
        let mut hits = Vec::new();
        if use_cache {
            for i in start..end {
                if let Some((g, id)) = gate {
                    if i & 0xFFF == 0 && g.load(Ordering::Relaxed) != id {
                        return None;
                    }
                }
                let v = source.get(i);
                if let Some(resolved) = self.seg.resolve(v as u64) {
                    let hit = match resolved {
                        ResolvedRow::Src(rec) => self.cache_match(spec, rec),
                        // 插入行不在列缓存中：走一次慢路径（通常极少）
                        ResolvedRow::Ins { .. } => self
                            .row_at(v as u64)
                            .is_some_and(|f| spec_matches(spec, &f)),
                    };
                    if hit {
                        hits.push(v);
                    }
                }
            }
            return Some(ChunkOut {
                hits,
                builds: Vec::new(),
                rows: end - start,
            });
        }

        let mut scan = FieldScan::new(
            spec,
            self.enc,
            &self.overlay,
            self.base_ncols,
            build_cols,
        );
        for i in start..end {
            if let Some((g, id)) = gate {
                if i & 0xFFF == 0 && g.load(Ordering::Relaxed) != id {
                    return None;
                }
            }
            let v = source.get(i);
            match self.seg.resolve(v as u64) {
                Some(ResolvedRow::Src(rec)) => {
                    let (s, e) =
                        self.idx.record_span(self.mmap.len(), rec as usize + 1);
                    scan.begin_src_record(rec);
                    let hit = scan.record_matches(rec, &self.mmap[s..e]);
                    scan.finish_src_record();
                    if hit {
                        hits.push(v);
                    }
                }
                Some(ResolvedRow::Ins { .. }) => {
                    if self
                        .row_at(v as u64)
                        .is_some_and(|f| spec_matches(spec, &f))
                    {
                        hits.push(v);
                    }
                }
                None => {}
            }
        }
        Some(ChunkOut {
            hits,
            builds: scan.into_builds(),
            rows: end - start,
        })
    }

    /// 合并一个列的各块缓存片段，得到按源记录号索引的连续投影。
    fn merge_builds(&self, chunks: &[BuildOut]) -> MergedBuild {
        let Some(head) = chunks.first() else {
            return MergedBuild::Invalid;
        };
        let col = head.col;
        let n_data = self.idx.n_records().saturating_sub(1);
        let mut data: Vec<u8> = Vec::new();
        let mut offs = vec![0u32; n_data + 1];
        let mut filled = 0usize;
        for b in chunks {
            if b.col != col {
                return MergedBuild::Invalid;
            }
            let first = b.first_rec as usize;
            if first > n_data || b.last_rec as usize >= n_data {
                return MergedBuild::Invalid;
            }
            if first > filled {
                // 块间空洞（被删除的源记录）→ 空值
                while filled < first {
                    offs[filled + 1] = data.len() as u32;
                    filled += 1;
                }
            } else if first < filled {
                return MergedBuild::Invalid; // 块重叠不应出现
            }
            for rec in first..=(b.last_rec as usize) {
                let i = rec - first;
                let (Some(&s), Some(&e)) = (b.offs.get(i), b.offs.get(i + 1))
                else {
                    return MergedBuild::Invalid;
                };
                let (s, e) = (s as usize, e as usize);
                if s > e || e > b.values.len() {
                    return MergedBuild::Invalid;
                }
                data.extend_from_slice(&b.values[s..e]);
                if data.len() > COL_CACHE_BUDGET || data.len() > u32::MAX as usize {
                    return MergedBuild::TooBig;
                }
                offs[rec + 1] = data.len() as u32;
                filled = rec + 1;
            }
        }
        while filled < n_data {
            offs[filled + 1] = data.len() as u32;
            filled += 1;
        }
        MergedBuild::Cache(ColCache { data, offs })
    }

    /// 条件涉及的所有列是否都已有缓存。
    fn cache_covers(&self, spec: &FilterSpec) -> bool {
        let mut any = false;
        for (col, _) in &spec.cols {
            any = true;
            if !self.col_cache.contains_key(col) {
                return false;
            }
        }
        if !spec.multi_text.is_empty() {
            any = true;
            for col in &spec.multi_cols {
                if !self.col_cache.contains_key(col) {
                    return false;
                }
            }
        }
        any
    }

    /// 按列缓存匹配一条源记录（所有条件列必须已缓存）。
    fn cache_match(&self, spec: &FilterSpec, rec: u32) -> bool {
        let rec = rec as usize;
        for (col, kw) in &spec.cols {
            let Some(cache) = self.col_cache.get(col) else {
                return false;
            };
            if memchr::memmem::find(cache.get(rec), kw.as_bytes()).is_none() {
                return false;
            }
        }
        if !spec.multi_text.is_empty() {
            let mut hit = false;
            for col in &spec.multi_cols {
                if let Some(cache) = self.col_cache.get(col) {
                    if memchr::memmem::find(cache.get(rec), spec.multi_text.as_bytes())
                        .is_some()
                    {
                        hit = true;
                        break;
                    }
                }
            }
            if !hit {
                return false;
            }
        }
        true
    }

    /// 删除某列缓存（该列发生编辑时）。
    fn cache_drop(&mut self, col: u32) {
        if let Some(c) = self.col_cache.remove(&col) {
            self.cache_bytes = self.cache_bytes.saturating_sub(c.size());
            self.cache_lru.retain(|&x| x != col);
        }
    }

    /// 装入列缓存：按 LRU 腾退，超预算或超 4 GiB 的单列永久放弃。
    fn cache_put(&mut self, col: u32, cache: ColCache) {
        let size = cache.size();
        if size > COL_CACHE_BUDGET {
            self.cache_skip.insert(col);
            return;
        }
        self.cache_drop(col);
        while self.cache_bytes + size > COL_CACHE_BUDGET {
            let Some(old) = self.cache_lru.pop_front() else {
                break;
            };
            if let Some(c) = self.col_cache.remove(&old) {
                self.cache_bytes = self.cache_bytes.saturating_sub(c.size());
            }
        }
        self.cache_bytes += size;
        self.cache_lru.push_back(col);
        self.col_cache.insert(col, cache);
    }

    /// 清空全部列缓存（重载/撤销重做时）。
    fn cache_clear_all(&mut self) {
        self.col_cache.clear();
        self.cache_lru.clear();
        self.cache_bytes = 0;
        self.cache_skip.clear();
    }

    /// 是否有生效的筛选结果。
    pub fn has_filter(&self) -> bool {
        self.filter.is_some()
    }

    /// 当前筛选命中数（无筛选时 0）。
    pub fn filter_len(&self) -> u32 {
        self.filter.as_ref().map_or(0, |f| f.len() as u32)
    }

    /// 清除筛选结果（列缓存保留，下次筛选仍可复用）。
    pub fn clear_filter(&mut self) {
        self.filter = None;
        self.last_spec = None;
    }

    /// 筛选命中分页取行。
    pub fn filter_page(&self, from: u32, count: u32) -> Vec<Vec<String>> {
        let Some(f) = &self.filter else { return Vec::new() };
        let hi = (from as usize + count as usize).min(f.len());
        f[from as usize..hi]
            .iter()
            .filter_map(|&v| self.row_at(v as u64))
            .collect()
    }

    // ---- 复制（大选区走引擎） ----

    /// 选区转 TSV（含引号转义，\r\n 行尾，对齐 csv.js serializeRecords）。
    pub fn copy_tsv(&self, r0: u64, r1: u64, c0: u32, c1: u32) -> Result<String, String> {
        let total = if self.partial.is_some() {
            self.indexed_data_rows()
        } else {
            self.seg.vrow_count()
        };
        if r1 < r0 || r1 > total {
            return Err("复制范围越界".into());
        }
        let mut out = String::new();
        if self.partial.is_some() {
            // 渐进索引期 vrow == 源记录号（无插删行）
            for v in r0..r1 {
                let fields = self.src_fields(v as u32);
                let row: Vec<String> = (c0..c1)
                    .map(|c| fields.get(c as usize).cloned().unwrap_or_default())
                    .collect();
                out.push_str(&serialize_record(&row, b'\t'));
            }
            return Ok(out);
        }
        let cum = self.seg_cum_lens();
        for v in r0..r1 {
            if let Some(fields) = self.row_at_fast(&cum, v) {
                let row: Vec<String> = (c0..c1)
                    .map(|c| fields.get(c as usize).cloned().unwrap_or_default())
                    .collect();
                out.push_str(&serialize_record(&row, b'\t'));
            }
        }
        Ok(out)
    }

    // ---- 列操作 ----

    /// 追加待执行列操作（渲染预览即时，物理重写延迟到 apply）。
    ///
    /// # Errors
    /// 后台索引未完成时拒绝（列映射会打乱索引期 vrow == 记录号的前提）。
    pub fn push_col_op(&mut self, op: ColOp) -> Result<(), String> {
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持列操作".into());
        }
        self.pending_cols.push(op);
        self.filter = None; // 列映射变化，旧筛选结果作废
        self.last_spec = None;
        Ok(())
    }

    /// 撤销最近一次待执行列操作。
    pub fn pop_col_op(&mut self) {
        self.pending_cols.pop();
    }

    /// 清空待执行列操作（用户放弃）。
    pub fn clear_col_ops(&mut self) {
        self.pending_cols.clear();
    }

    // ---- 序列化与落盘 ----

    /// 保存：流式重写原文件（覆盖层/插入/删除合并）。
    /// 进度回调 `(加权done, total, 阶段, 行数)`：加权把总体进度切给
    /// 流式 0-70% / 落盘 70-78% / 重建索引至 100%（见 PHASE_* 常量注释）。
    /// 保存后重建 mmap 与索引；撤销栈保留（操作按稳定记录号寻址，仍有效）。
    pub fn save<F: FnMut(u64, u64, u8, u64)>(&mut self, mut progress: F) -> Result<(), String> {
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，请稍候再保存".into());
        }
        // 整表重写数十秒：本线程降为背景优先级，让出 UI/系统调度。
        let _bg = bg::BackgroundGuard::enter();
        let tmp = tmp_path_for(&self.path);
        let out = fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        let mut w = BufWriter::new(out);
        if self.has_bom {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        let header = self.view_headers();
        w.write_all(&encode_record(&header, b',', self.enc))
            .map_err(|e| e.to_string())?;
        let total = self.seg.vrow_count();
        // 有覆盖层编辑的源行（透传判定的"不洁净"集；覆盖层通常极小）
        let edited: std::collections::HashSet<u32> =
            self.overlay.keys().map(|k| k.0).collect();
        let mut done = 0u64;
        let mut last_emit: Option<Instant> = None;
        for s in &self.seg.segs {
            match s.kind {
                SegKind::Src { start, len } => {
                    // 字节透传：无覆盖层编辑的连续行整段照抄原文件字节
                    // （引号风格/空格/末行无换行全部原样，且是顺序拷贝不重编码，
                    // 删行保存从"逐行解析重编码"变"按洞拷贝"）；遇编辑行落编码。
                    let seg_end = start + len;
                    let mut run: Option<u32> = None;
                    for rec in start..seg_end {
                        if edited.contains(&rec) {
                            if let Some(rs) = run.take() {
                                self.write_raw_range(&mut w, rs, rec)?;
                            }
                            let f = self.src_fields(rec);
                            w.write_all(&encode_record(&f, b',', self.enc))
                                .map_err(|e| e.to_string())?;
                        } else if run.is_none() {
                            run = Some(rec);
                        }
                        done += 1;
                        // 16 行粒度过闸：磁盘拖慢时进度仍按 80ms 节拍更新
                        // （旧 65536 行粒度在龟速段可能十分钟不刷新）。
                        if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                            progress(stream_weighted(done), total, PHASE_STREAM, done);
                        }
                    }
                    if let Some(rs) = run.take() {
                        self.write_raw_range(&mut w, rs, seg_end)?;
                    }
                }
                SegKind::Ins { block } => {
                    if let Some(Some(rows)) = self.seg.blocks.get(block as usize) {
                        for row in rows {
                            let mut f = row.clone();
                            let n = self.base_ncols;
                            if f.len() < n {
                                f.resize(n, String::new());
                            } else if f.len() > n {
                                f.truncate(n);
                            }
                            w.write_all(&encode_record(&f, b',', self.enc))
                                .map_err(|e| e.to_string())?;
                            done += 1;
                            if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                                progress(stream_weighted(done), total, PHASE_STREAM, done);
                            }
                        }
                    }
                }
            }
        }
        // 阶段交接无条件各发一次：小文件也保证契约完整（有阶段 0/1 事件）。
        progress(stream_weighted(done), total, PHASE_STREAM, done);
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        progress(flushed_weighted(total), total, PHASE_FLUSH, done);
        self.replace_with_retry(&tmp)?;
        progress(renamed_weighted(total), total, PHASE_FLUSH, done);
        self.reopen_after_write()?;
        progress(total, total, PHASE_REINDEX, total); // 收尾必到满，前端进度条归零前显示 100%
        Ok(())
    }

    /// 执行待执行的列操作：流式重写（物理列映射），完成后重建会话。
    /// 此操作不可撤销（撤销栈清空）。进度回调契约同 `save`。
    pub fn apply_col_ops<F: FnMut(u64, u64, u8, u64)>(
        &mut self,
        mut progress: F,
    ) -> Result<(), String> {
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持列操作".into());
        }
        if self.pending_cols.is_empty() {
            return Ok(());
        }
        // 整表物理重写数十秒：本线程降为背景优先级，让出 UI/系统调度。
        let _bg = bg::BackgroundGuard::enter();
        let new_headers = self.view_headers();
        // 视图→物理映射（apply 后 view == 物理，映射只是本次重写用）
        let ncols = self.view_ncols();
        let mapping: Vec<Option<u32>> = (0..ncols).map(|vc| self.view_to_phys(vc)).collect();

        let tmp = tmp_path_for(&self.path);
        let out = fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        let mut w = BufWriter::new(out);
        if self.has_bom {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        w.write_all(&encode_record(&new_headers, b',', self.enc))
            .map_err(|e| e.to_string())?;
        let total = self.seg.vrow_count();
        let mut done = 0u64;
        let mut last_emit: Option<Instant> = None;
        for s in &self.seg.segs {
            match s.kind {
                SegKind::Src { start, len } => {
                    for rec in start..start + len {
                        let base = self.src_fields(rec);
                        let mapped: Vec<String> = mapping
                            .iter()
                            .map(|m| match m {
                                Some(p) => base.get(*p as usize).cloned().unwrap_or_default(),
                                None => String::new(),
                            })
                            .collect();
                        w.write_all(&encode_record(&mapped, b',', self.enc))
                            .map_err(|e| e.to_string())?;
                        done += 1;
                        // 16 行粒度过闸：磁盘拖慢时进度仍按 80ms 节拍更新。
                        if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                            progress(stream_weighted(done), total, PHASE_STREAM, done);
                        }
                    }
                }
                SegKind::Ins { block } => {
                    if let Some(Some(rows)) = self.seg.blocks.get(block as usize) {
                        for row in rows {
                            let mapped: Vec<String> = mapping
                                .iter()
                                .map(|m| match m {
                                    Some(p) => {
                                        row.get(*p as usize).cloned().unwrap_or_default()
                                    }
                                    None => String::new(),
                                })
                                .collect();
                            w.write_all(&encode_record(&mapped, b',', self.enc))
                                .map_err(|e| e.to_string())?;
                            done += 1;
                            if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                                progress(stream_weighted(done), total, PHASE_STREAM, done);
                            }
                        }
                    }
                }
            }
        }
        // 阶段交接无条件各发一次：小文件也保证契约完整（有阶段 0/1 事件）。
        progress(stream_weighted(done), total, PHASE_STREAM, done);
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        progress(flushed_weighted(total), total, PHASE_FLUSH, done);
        self.replace_with_retry(&tmp)?;
        progress(renamed_weighted(total), total, PHASE_FLUSH, done);

        // 全量重建：列结构变化，编辑层与撤销历史一并失效
        let mut fresh = Self::open(&self.path)?;
        fresh.base_headers = new_headers;
        fresh.base_ncols = ncols as usize;
        *self = fresh;
        progress(total, total, PHASE_REINDEX, total); // 收尾必到满
        Ok(())
    }

    /// 重新映射原文件并重建行索引（不动 Segment/覆盖层/撤销栈）。
    /// 用于保存换名失败后的读路径恢复。
    fn remap(&mut self) -> Result<(), String> {
        let t0 = Instant::now();
        let file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        self.cancel.store(true, Ordering::Relaxed); // 防御：旧索引线程不要再用旧映射
        self.partial = None;
        self.idx = build_index(bytes);
        self.mmap = mmap;
        self.index_ms = t0.elapsed().as_millis() as u64;
        Ok(())
    }

    /// 用写好的临时文件替换原文件。
    ///
    /// 换名前先摘除本进程的映射句柄（自己的只读句柄不挡换名，也不给
    /// 错误排查添变量），失败按 200ms 间隔重试 5 次（覆盖杀软/索引器等
    /// 瞬时占用）；仍失败则映回原文件保持会话可用（编辑层/撤销栈不动），
    /// 临时文件原样保留，错误信息给出路径供手动恢复。
    fn replace_with_retry(&mut self, tmp: &Path) -> Result<(), String> {
        // 占位匿名映射：mmap 字段不留空，1 字节开销
        let dummy = memmap2::MmapMut::map_anon(1)
            .and_then(|m| m.make_read_only())
            .map_err(|e| e.to_string())?;
        // 旧映射解除、文件句柄关闭（保存前已 refresh_indexed，无后台线程共享）
        drop(std::mem::replace(&mut self.mmap, Arc::new(dummy)));
        let mut last: Option<std::io::Error> = None;
        for _ in 0..5 {
            match fs::rename(tmp, &self.path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
        // 换名失败：映回原文件恢复读路径（seg/覆盖层/撤销栈原样保留）
        if let Err(re) = self.remap() {
            return Err(format!(
                "替换原文件失败：{}\n且原文件重新映射失败：{}",
                last.map(|e| e.to_string()).unwrap_or_default(),
                re
            ));
        }
        Err(format!(
            "替换原文件失败：{}\n新文件已完整写在临时文件里（数据未丢失）：{}\n\
             可关闭占用原文件的程序后重试保存，或手动把该临时文件改名为原文件。",
            last.map(|e| e.to_string()).unwrap_or_default(),
            tmp.display()
        ))
    }

    /// 写盘后重建 mmap/索引；编辑层已落盘故清空，撤销栈保留。
    fn reopen_after_write(&mut self) -> Result<(), String> {
        let t0 = Instant::now();
        let n_data = self.seg.vrow_count();
        let file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        let idx = build_index(bytes);
        if idx.n_records() as u64 != n_data + 1 {
            return Err("写入后行数校验失败".into());
        }
        self.mmap = mmap;
        self.idx = idx;
        self.partial = None;
        self.index_ms = t0.elapsed().as_millis() as u64;
        self.total_bytes = meta.len();
        self.seg = Segments::new(n_data as u32);
        self.overlay.clear();
        self.filter = None;
        self.last_spec = None;
        self.cache_clear_all();
        self.base_size = meta.len();
        self.base_mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        Ok(())
    }
}

/// 筛选匹配（语义对齐 grid.js _refilter：多重 OR 叠加逐列 AND；
/// needle 与 multi_text 均已小写化，单元格现场小写化）。
fn spec_matches(spec: &FilterSpec, fields: &[String]) -> bool {
    if !spec.multi_text.is_empty() {
        let hit = spec.multi_cols.iter().any(|&c| {
            fields
                .get(c as usize)
                .is_some_and(|f| f.to_lowercase().contains(spec.multi_text.as_str()))
        });
        if !hit {
            return false;
        }
    }
    for (c, kw) in &spec.cols {
        if !fields
            .get(*c as usize)
            .is_some_and(|f| f.to_lowercase().contains(kw.as_str()))
        {
            return false;
        }
    }
    true
}

/// 单条记录序列化（RFC 4180 转义 + \r\n 行尾，对齐 csv.js serializeRecords）。
fn serialize_record(fields: &[String], delimiter: u8) -> String {
    let d = delimiter as char;
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(d);
        }
        let needs_quote = f.contains(d) || f.contains('"') || f.contains('\r') || f.contains('\n');
        if needs_quote {
            out.push('"');
            for ch in f.chars() {
                if ch == '"' {
                    out.push('"');
                }
                out.push(ch);
            }
            out.push('"');
        } else {
            out.push_str(f);
        }
    }
    out.push_str("\r\n");
    out
}

/// 按编码序列化并转字节（UTF-8 带原文编码；GBK 重新编码）。
fn encode_record(fields: &[String], delimiter: u8, enc: Encoding) -> Vec<u8> {
    let s = serialize_record(fields, delimiter);
    match enc {
        Encoding::Utf8 => s.into_bytes(),
        Encoding::Gbk => encoding_rs::GBK.encode(&s).0.into_owned(),
    }
}

/// 同目录临时文件路径（写盘成功后原子换名覆盖原文件）。
fn tmp_path_for(path: &Path) -> PathBuf {
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let mut tmp = name;
    tmp.push(".simpletable-tmp");
    dir.join(tmp)
}
