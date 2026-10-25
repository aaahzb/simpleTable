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

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::baseline::{BaselineTransform, RowId};
use crate::history::{
    Approval, BudgetChange, Cost, EditOutcome, FixedPointKind, FixedPointRequest,
    HistoryEntry, HistoryError, HistoryLedger, HistoryStats, Prepared, PreparedHistory,
    SharedCharge,
    DEFAULT_HISTORY_BUDGET_BYTES,
};
use crate::schema::{ColumnId, ColumnStorage, Schema};
use crate::spool::{history_text_bytes, RowSnapshot, SessionSpool};
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

/// 判断一条 CSV 记录中第 `target` 个字段是否有内容。
///
/// 与 `parse_record` 使用同一套宽松引号方言，但只跟踪“是否出现过内容”，
/// 不分配字段数组；用于大文件里逐行判断某列是否为空（Ctrl+↑/↓ 边缘查找）。
/// 空格算内容（与 Excel 一致），引号空串 `""` 算空。
///
/// # Parameters
/// - `bytes`: 单条记录的字节切片（可含引号内换行与结尾换行符）。
/// - `delimiter`: 字段分隔符（CSV 为 b','）。
/// - `target`: 目标字段下标（0 基）。
///
/// # Returns
/// 目标字段存在且含有内容时为 true。
fn field_nonempty(bytes: &[u8], delimiter: u8, target: usize) -> bool {
    let mut field = 0usize;
    let mut in_quotes = false;
    let mut has_content = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_quotes {
            if ch == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    // "" 转义产生一个字面量引号：算内容
                    has_content = true;
                    i += 2;
                } else {
                    in_quotes = false;
                    i += 1;
                }
            } else {
                // 引号内任意字节（含内嵌换行与多字节字符）都是内容
                has_content = true;
                i += 1;
            }
            continue;
        }
        if ch == b'"' {
            in_quotes = true;
            i += 1;
            continue;
        }
        if ch == delimiter {
            if field == target {
                return has_content;
            }
            field += 1;
            has_content = false;
            i += 1;
            continue;
        }
        if ch == b'\r' || ch == b'\n' {
            // parse_record 在首个引号外换行处结束记录
            return field == target && has_content;
        }
        has_content = true;
        i += 1;
    }
    field == target && has_content
}

/// Excel 风格纵向边缘查找的索引层实现（与 JS `findVerticalEdge` 同语义）。
///
/// # Parameters
/// - `start`: 当前行索引（越界时夹取）。
/// - `count`: 可导航行数。
/// - `down`: true=向下；false=向上。
/// - `is_nonempty`: 判断某行目标列是否非空。
///
/// # Returns
/// 目标行索引；无可导航行时为 None。
fn find_vertical_edge_by<F>(
    start: u64,
    count: u64,
    down: bool,
    is_nonempty: F,
) -> Option<u64>
where
    F: Fn(u64) -> bool,
{
    if count == 0 {
        return None;
    }
    let last = count - 1;
    let begin = start.min(last);
    let step: i64 = if down { 1 } else { -1 };
    let fallback = if down { last } else { 0 };
    let in_range = |i: i64| i >= 0 && i <= last as i64;

    // 当前格非空：先走完连续非空块；整块只有当前格时再越过空档找下一块。
    if is_nonempty(begin) {
        let mut edge = begin;
        let mut i = begin as i64 + step;
        while in_range(i) && is_nonempty(i as u64) {
            edge = i as u64;
            i += step;
        }
        if edge != begin {
            return Some(edge);
        }
        while in_range(i) && !is_nonempty(i as u64) {
            i += step;
        }
        return Some(if in_range(i) { i as u64 } else { fallback });
    }

    // 当前格为空：直接找该方向的下一个非空格。
    let mut i = begin as i64 + step;
    while in_range(i) && !is_nonempty(i as u64) {
        i += step;
    }
    Some(if in_range(i) { i as u64 } else { fallback })
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

    #[test]
    fn field_nonempty_matches_parsed_field_semantics() {
        assert!(!field_nonempty(b"", b',', 0));
        assert!(field_nonempty(b"solo", b',', 0));
        assert!(!field_nonempty(b"a,", b',', 1));
        // Excel 语义：空格是内容，不是空单元格
        assert!(field_nonempty(b"a, ", b',', 1));
        // 引号空串为空；转义引号本身是内容
        assert!(!field_nonempty(b"\"\",x", b',', 0));
        assert!(field_nonempty(b"\"\"\"\"", b',', 0));
        // 引号内逗号/换行属于同一字段
        assert!(field_nonempty(b"\"a,b\",c", b',', 0));
        assert!(field_nonempty("\"line1\nline2\",x".as_bytes(), b',', 0));
        // 字段数不足与记录结束：不能把下一条记录算进来
        assert!(!field_nonempty(b"\"a,b\",c", b',', 2));
        assert!(!field_nonempty(b"a,b\r\nc", b',', 2));
        assert!(field_nonempty(b"a,b\r\nc", b',', 1));
    }

    #[test]
    fn vertical_edge_helper_matches_excel_semantics() {
        let v = [true, true, false, false, true, true];
        let get = |i: u64| v[i as usize];
        assert_eq!(find_vertical_edge_by(0, 6, true, get), Some(1));
        assert_eq!(find_vertical_edge_by(1, 6, true, get), Some(4));
        assert_eq!(find_vertical_edge_by(5, 6, true, get), Some(5));
        // 双空档：从下块首格向上越过两格空档，落到上块末格
        assert_eq!(find_vertical_edge_by(4, 6, false, get), Some(1));
        assert_eq!(find_vertical_edge_by(3, 6, false, get), Some(1));
        assert_eq!(find_vertical_edge_by(1, 6, false, get), Some(0));
        let v2 = [true, true, false, true, true];
        let get2 = |i: u64| v2[i as usize];
        assert_eq!(find_vertical_edge_by(4, 5, false, get2), Some(3));
        assert_eq!(find_vertical_edge_by(3, 5, false, get2), Some(1));
        let empty = [false, false, false];
        let none = |i: u64| empty[i as usize];
        assert_eq!(find_vertical_edge_by(0, 3, true, none), Some(2));
        assert_eq!(find_vertical_edge_by(2, 3, false, none), Some(0));
        assert_eq!(find_vertical_edge_by(0, 0, true, |_| true), None);
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
        assert!(!m.index_sync, "后台索引未完成时不得标记为同步完成");
        assert!(m.open_ms <= m.elapsed_ms);

        let rows = bf.rows(0, 100);
        assert_eq!(rows.len(), 3, "未索引部分不得返回");
        assert_eq!(rows[2], vec!["0002", "x"]);

        // 索引期允许仿编辑，但结构变更/筛选/保存/列操作必须拒绝
        bf.set_cell(2, 1, "EDITED".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED");
        assert!(bf.insert_rows(0, None, 1, Approval::deny()).is_err());
        assert!(bf.remove_rows(vec![0], Approval::deny()).is_err());
        assert!(bf.run_filter(&FilterSpec::default()).is_err());
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).is_err());
        assert!(bf
            .push_col_op(ColOp::Rename { col: 0, name: "A".into() })
            .is_err());

        // 后台补齐（测试里同步驱动），完成瞬间无需 refresh 也能读全量
        bf.drive_index_for_test();
        let m = bf.meta();
        assert!(!m.indexing);
        assert_eq!(m.rows, 100);
        assert!(!m.index_sync, "后台补齐的索引不应被标成「随首屏完成」");
        assert!(m.index_ms <= m.elapsed_ms);
        assert_eq!(bf.rows(0, 1000).len(), 100);
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED", "覆盖层跨索引完成保留");

        // 并入正式索引后行为不变，且重复调用幂等
        bf.refresh_indexed();
        assert!(bf.partial.is_none());
        bf.refresh_indexed();
        assert_eq!(bf.meta().rows, 100);
        assert_eq!(bf.rows(2, 1)[0][1], "EDITED");
        assert!(bf.insert_rows(100, None, 1, Approval::deny()).is_ok(), "完成后恢复结构操作");

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
        assert!(m.index_sync, "小文件全量索引应标记为「随打开同步完成」");
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

    /// 纵向边缘夹具：flag 列呈 [1,2,空,空,3,4]。
    fn edge_gap_bytes() -> Vec<u8> {
        b"id,flag\r\nA,1\r\nB,2\r\nC,\r\nD,\r\nE,3\r\nF,4\r\n".to_vec()
    }

    #[test]
    fn big_find_edge_contiguous_gap_and_boundaries() {
        let p = tmp_csv("edgegap", &edge_gap_bytes());
        let bf = BigFile::open(&p).unwrap();
        let find = |from: u64, down: bool| {
            bf.find_vertical_edge(from, 1, down, false).unwrap()
        };
        assert_eq!(find(0, true), Some(1), "连续块末格");
        assert_eq!(find(1, true), Some(4), "边缘再按一次越过空档");
        assert_eq!(find(4, true), Some(5), "下一条块末格");
        assert_eq!(find(5, true), Some(5), "已是末行不再移动");
        assert_eq!(find(5, false), Some(4), "向上到连续块首格");
        assert_eq!(find(4, false), Some(1), "向上越过空档");
        assert_eq!(find(1, false), Some(0));
        assert_eq!(find(2, true), Some(4), "空格向下找非空");
        assert_eq!(find(2, false), Some(1), "空格向上找非空");
        assert!(bf.find_vertical_edge(0, 2, true, false).is_err(), "列号越界应报错");
        assert_eq!(bf.find_vertical_edge(0, 1, true, false).unwrap(), Some(1));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_find_edge_sees_overlay_and_inserted_rows() {
        let p = tmp_csv("edgeedit", &edge_gap_bytes());
        let mut bf = BigFile::open(&p).unwrap();
        // 覆盖层：把原来的空格改成非空，导航必须看见
        bf.set_cell(2, 1, "x".into(), Approval::deny()).unwrap();
        assert_eq!(bf.find_vertical_edge(1, 1, true, false).unwrap(), Some(2));
        // 插入行带值：被视为新的连续块，可被导航命中
        bf.insert_rows(3, Some(vec![vec!["X".into(), "y".into()]]), 0, Approval::deny())
            .unwrap();
        assert_eq!(bf.find_vertical_edge(2, 1, true, false).unwrap(), Some(3));
        // 插入行之后原空行仍在：从插入行继续向下应越过它
        assert_eq!(bf.find_vertical_edge(3, 1, true, false).unwrap(), Some(5));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_find_edge_filtered_uses_visible_rows() {
        let p = tmp_csv(
            "edgefilter",
            b"id,flag\r\nkeepA,1\r\nkeepB,2\r\nkeepC,\r\nkeepD,\r\ndropE,3\r\nkeepF,4\r\n",
        );
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.run_filter(&spec_one(0, "keep")).unwrap(), 5);
        // 可见 flag 序列：[1, 2, 空, 空, 4]（dropE 被筛掉）
        assert_eq!(bf.find_vertical_edge(0, 1, true, true).unwrap(), Some(1));
        assert_eq!(bf.find_vertical_edge(1, 1, true, true).unwrap(), Some(4));
        assert_eq!(bf.find_vertical_edge(4, 1, true, true).unwrap(), Some(4));
        assert_eq!(bf.find_vertical_edge(4, 1, false, true).unwrap(), Some(1));
        assert_eq!(
            bf.filter_page(4, 1).rows[0][0],
            "keepF",
            "命中序号 4 应映射到 keepF"
        );
        // 没有筛选结果时返回 None，不应误扫全表
        bf.clear_filter();
        assert_eq!(bf.find_vertical_edge(0, 1, true, true).unwrap(), None);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_find_edge_sorted_uses_display_order() {
        let p = tmp_csv(
            "edgesort",
            b"id,val\r\na,\r\nb,x\r\nc,\r\nd,y\r\n",
        );
        let mut bf = load_ram(&p);
        // 排序前：空格在第 0 行，Ctrl+↓ 落到 b(x)
        assert_eq!(bf.find_vertical_edge(0, 1, true, false).unwrap(), Some(1));
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap();
        // 排序后显示顺序 d,c,b,a → val [y, 空, x, 空]
        assert_eq!(bf.find_vertical_edge(0, 1, true, false).unwrap(), Some(2));
        assert_eq!(bf.find_vertical_edge(2, 1, true, false).unwrap(), Some(3));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_find_edge_headerless_first_row_is_data() {
        let p = tmp_csv("edgehl", b",\r\nA,1\r\nB,2\r\n");
        let bf = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf.rows(0, 1)[0], vec!["", ""], "无列名模式首行是数据");
        assert_eq!(bf.find_vertical_edge(0, 1, true, false).unwrap(), Some(1));
        assert_eq!(bf.find_vertical_edge(1, 1, true, false).unwrap(), Some(2));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_find_edge_decodes_gbk_fields() {
        let (bytes, _, _) = encoding_rs::GBK.encode("id,flag\r\n甲,\r\n乙,1\r\n");
        let p = tmp_csv("edgegbk", &bytes);
        let bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().encoding, "gbk");
        assert_eq!(bf.find_vertical_edge(0, 1, true, false).unwrap(), Some(1));
        fs::remove_file(&p).ok();
    }


    #[test]
    fn source_edit_history_uses_backing_reference() {
        let p = tmp_csv("source_value_ref", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "Changed".into(), Approval::deny()).unwrap();
        let Some(Op::SetCell { old, new, .. }) = bf.history.undo_op() else {
            panic!("单格编辑必须产生 SetCell 历史");
        };
        assert!(matches!(
            old,
            ValueRef::Source {
                row: RowId::Source(1),
                col: ColumnId(0),
                utf8_len: 5,
                index_epoch: 0,
                ..
            }
        ));
        assert!(matches!(new, ValueRef::Text(text) if text.text.as_ref() == "Changed"));
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 1)[0][0], "Changed");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_set_cell_undo_redo_src_and_ins() {
        let p = tmp_csv("cell", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 源行编辑走覆盖层
        bf.set_cell(1, 0, "BOB".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(1, 1)[0][0], "BOB");
        assert!(bf.meta().can_undo);
        bf.undo();
        assert_eq!(bf.rows(1, 1)[0][0], "bob");
        bf.redo();
        assert_eq!(bf.rows(1, 1)[0][0], "BOB");
        // 撤销后再改 + 重做栈被清
        bf.undo();
        bf.set_cell(1, 0, "b2".into(), Approval::deny()).unwrap();
        assert!(!bf.meta().can_redo);
        // 插入行直接写块
        bf.insert_rows(0, Some(vec![vec!["X".into(), "1".into(), "Y".into()]]), 0, Approval::deny())
            .unwrap();
        bf.set_cell(0, 1, "99".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(0, 1)[0][1], "99");
        fs::remove_file(&p).ok();
    }

    /// 重做插行会换块号：其后的新行格编辑必须重绑定，不能写入死块。
    #[test]
    fn insert_rows_redo_rebinds_following_cell_edit() {
        let p = tmp_csv("redo_rebind_ins", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.insert_rows(
            0,
            Some(vec![vec!["INS".into(), "".into(), "".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.set_cell(0, 0, "EDITED".into(), Approval::deny()).unwrap();
        assert!(bf.undo(), "撤销新行格编辑");
        assert!(bf.undo(), "撤销插行");
        assert!(bf.redo(), "重做插行");
        assert!(bf.redo(), "重做新行格编辑");
        assert_eq!(bf.rows(0, 1)[0][0], "EDITED");
        fs::remove_file(&p).ok();
    }

    /// 插行逆操作的位置或内容不匹配时，不得假成功或移动历史栈。
    #[test]
    fn insert_rows_undo_validation_failure_keeps_history() {
        let p = tmp_csv("undo_insert_validate", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.insert_rows(
            0,
            Some(vec![vec!["REAL".into(), "".into(), "".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        let revision = bf.revision;
        let block = match bf.history.undo_op().unwrap() {
            Op::InsertRows { block, .. } => *block,
            other => panic!("应为插行记录：{other:?}"),
        };
        bf.seg.blocks[block as usize].as_mut().unwrap()[0][0] = "MISMATCH".into();
        assert!(!bf.undo(), "内容不匹配必须拒绝逆操作");
        assert_eq!(bf.meta().rows, 6);
        // 故障注入直接修改实时块；失败的撤销必须保留这个现场。
        assert_eq!(bf.rows(0, 1)[0][0], "MISMATCH");
        assert!(bf.meta().can_undo, "失败后原撤销记录仍在");
        assert!(!bf.meta().can_redo, "失败后不得产生重做记录");
        assert_eq!(bf.revision, revision);
        fs::remove_file(&p).ok();
    }

    /// 多格批量修改跨源行/插入行且非连续：整批只占一步并反复往返。
    #[test]
    fn set_cells_one_step_mixed_identities_and_roundtrips() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.insert_rows(
            2,
            Some(vec![vec!["TMP".into(), "1".into(), "X".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        let original = bf.rows(0, 6);
        assert!(bf
            .set_cells(vec![
                (0, 0, "A2".into()),
                (2, 1, "99".into()),
                (4, 2, "D City".into()),
                (0, 2, "A City".into()),
            ], Approval::deny())
            .unwrap().changed);
        let changed = bf.rows(0, 6);
        assert_eq!(changed[0][0], "A2");
        assert_eq!(changed[0][2], "A City");
        assert_eq!(changed[2][1], "99");
        assert_eq!(changed[4][2], "D City");
        assert_eq!(bf.history_stats().undo_count, 2, "插行一条 + 整批一条");
        for _ in 0..2 {
            assert!(bf.undo(), "一次撤销整批");
            assert_eq!(bf.rows(0, 6), original);
            assert!(bf.redo(), "一次重做整批");
            assert_eq!(bf.rows(0, 6), changed);
        }
    }

    /// 同一稳定格重复出现时后者胜；最终回到批前值的项不进入快照。
    #[test]
    fn set_cells_duplicate_last_wins_and_final_noop_filtered() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        assert!(bf
            .set_cells(vec![
                (0, 0, "TEMP".into()),
                (1, 1, "26".into()),
                (0, 0, "Alice2".into()),
                (0, 0, "Alice".into()),
                (2, 2, "Beijing".into()),
            ], Approval::deny())
            .unwrap().changed);
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        assert_eq!(bf.rows(1, 1)[0][1], "26");
        assert_eq!(bf.history_stats().undo_count, 1);
        let Op::SetCells { changes } = bf.history.undo_op().unwrap() else {
            panic!("应形成一条批量格快照");
        };
        assert_eq!(changes.len(), 1, "只有 bob 年龄实际变化");
        assert!(matches!(
            &changes[0].old,
            ValueRef::Source { row: RowId::Source(2), col: ColumnId(1), utf8_len: 2, .. }
        ));
        assert!(matches!(&changes[0].new, ValueRef::Text(text) if text.text.as_ref() == "26"));
        assert!(bf.undo());
        assert_eq!(bf.rows(1, 1)[0][1], "25");
    }

    /// 空批量和最终全同值都不增 revision、不压栈，也不清既有 redo。
    #[test]
    fn set_cells_empty_and_noop_preserve_redo_and_revision() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 0, "Z".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        let revision = bf.revision;
        let redo = bf.history.redo_op().cloned();
        assert!(!bf.set_cells(Vec::new(), Approval::deny()).unwrap().changed);
        assert!(!bf
            .set_cells(vec![
                (0, 0, "Alice".into()),
                (0, 0, "TEMP".into()),
                (0, 0, "Alice".into()),
                (0, 1, "30".into()),
            ], Approval::deny())
            .unwrap().changed);
        assert_eq!(bf.revision, revision);
        assert_eq!(bf.history.redo_op(), redo.as_ref());
        assert!(bf.history_stats().undo_count == 0);
        assert!(bf.redo(), "既有重做记录必须仍可使用");
        assert_eq!(bf.rows(0, 1)[0][0], "Z");
    }

    /// 任一目标行/列非法时整批拒绝，已解析的前置目标也不得写入。
    #[test]
    fn set_cells_invalid_row_or_col_is_atomic() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 0, "Before".into(), Approval::deny()).unwrap();
        let before = bf.rows(0, 5);
        let undo = bf.history.undo_op().cloned();
        let redo = bf.history.redo_op().cloned();
        let revision = bf.revision;
        let err = bf
            .set_cells(vec![(0, 1, "31".into()), (99, 0, "X".into())], Approval::deny())
            .unwrap_err();
        assert!(err.contains("行号"), "{err}");
        let err = bf
            .set_cells(vec![(0, 1, "31".into()), (1, 3, "X".into())], Approval::deny())
            .unwrap_err();
        assert!(err.contains("列号"), "{err}");
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.history.undo_op(), undo.as_ref());
        assert_eq!(bf.history.redo_op(), redo.as_ref());
        assert_eq!(bf.revision, revision);
    }

    /// 单步完整成本超过可配置预算时返回固定点请求，写入前保持原状。
    #[test]
    fn set_cells_snapshot_budget_uses_utf8_bytes_and_rejects() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_history_budget(4_096).unwrap();
        let before = bf.rows(0, 5);
        let revision = bf.revision;
        let too_big = "界".repeat(700_000);
        let outcome = bf
            .set_cells(
                vec![(0, 0, too_big), (1, 0, "NO".into())],
                Approval::deny(),
            )
            .unwrap();
        assert!(!outcome.changed);
        let request = outcome.fixed_point.unwrap();
        assert!(request.required_bytes > request.budget_bytes);
        assert_eq!(request.budget_bytes, 4_096);
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.history_stats().undo_count == 0 && bf.history_stats().redo_count == 0);
        assert_eq!(bf.revision, revision);
    }

    /// 多格操作只占一步；小操作不再受固定 200 条上限约束。
    #[test]
    fn set_cells_uses_one_undo_slot_without_fixed_count_cap() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        for i in 0..205 {
            assert!(bf
                .set_cells(
                    vec![(0, 0, i.to_string()), (1, 0, format!("b{i}"))],
                    Approval::deny(),
                )
                .unwrap()
                .changed);
        }
        assert_eq!(bf.history_stats().undo_count, 205);
        for _ in 0..205 {
            assert!(bf.undo());
        }
        assert!(!bf.undo());
    }

    /// 不规则粘贴只写输入实际覆盖的格子，短行尾部保持原值。
    #[test]
    fn paste_rows_ragged_existing_region_one_step() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let original = bf.rows(0, 5);
        let out = bf
            .paste_rows(
                1,
                1,
                vec![
                    vec!["26".into(), "SH".into()],
                    vec!["36".into()],
                    vec!["28".into(), "GZ".into()],
                ],
                Approval::deny(),
            )
            .unwrap();
        assert!(out.changed);
        assert_eq!(out.inserted_rows, 0);
        let pasted = bf.rows(0, 5);
        assert_eq!(pasted[1], vec!["bob", "26", "SH"]);
        assert_eq!(pasted[2], vec!["Carol", "36", "Beijing"]);
        assert_eq!(pasted[3], vec!["dave", "28", "GZ"]);
        assert_eq!(bf.history_stats().undo_count, 1);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), original);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), pasted);
    }

    /// 已提交 Inserted 列的逻辑行必须转换到物理存储槽，并可完整撤销重做。
    #[test]
    fn insert_rows_use_inserted_storage_slots() {
        let mut bf = BigFile::from_bytes(b"A,B
a,b
".to_vec(), false).unwrap();
        let mut schema = (*bf.schema).clone();
        let inserted = schema.insert(1, "X".into()).unwrap();
        assert_eq!(schema.storage(inserted), Some(ColumnStorage::Inserted(0)));
        bf.schema = Arc::new(schema);

        bf.insert_rows(
            1,
            Some(vec![vec!["a2".into(), "x2".into(), "b2".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        let expected = vec!["a2", "x2", "b2"];
        assert_eq!(bf.rows(1, 1)[0], expected);
        assert!(bf.undo());
        assert_eq!(bf.meta().rows, 1);
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1)[0], expected);
    }

    /// 扩行粘贴按起始稳定列写入逻辑行，再映射到 Inserted 物理槽。
    #[test]
    fn paste_append_uses_inserted_storage_slots() {
        let mut bf = BigFile::from_bytes(b"A,B
a,b
".to_vec(), false).unwrap();
        let mut schema = (*bf.schema).clone();
        schema.insert(1, "X".into()).unwrap();
        bf.schema = Arc::new(schema);

        let out = bf
            .paste_rows(1, 1, vec![vec!["x2".into(), "b2".into()]], Approval::deny())
            .unwrap();
        assert_eq!(out.inserted_rows, 1);
        let expected = vec!["", "x2", "b2"];
        assert_eq!(bf.rows(1, 1)[0], expected);
        assert!(bf.undo());
        assert_eq!(bf.meta().rows, 1);
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1)[0], expected);
    }

    /// 空白插入行保持稀疏，只在实际写入目标列时扩到对应物理槽。
    #[test]
    fn blank_rows_keep_inserted_storage_sparse() {
        let mut bf = BigFile::from_bytes(b"A,B
a,b
".to_vec(), false).unwrap();
        let mut schema = (*bf.schema).clone();
        schema.insert(1, "X".into()).unwrap();
        bf.schema = Arc::new(schema);
        bf.insert_rows(1, None, 1, Approval::deny()).unwrap();

        let ResolvedRow::Ins { block, row } = bf.seg.resolve(1).unwrap() else {
            panic!("应解析为插入行");
        };
        let physical_len = |bf: &BigFile| {
            bf.seg.blocks[block as usize].as_ref().unwrap()[row as usize].len()
        };
        assert_eq!(physical_len(&bf), 0, "空白行不得预占所有物理槽");
        bf.set_cell(1, 2, "b2".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(1, 1)[0], vec!["", "", "b2"]);
        assert_eq!(physical_len(&bf), 2, "Source(1) 只需扩到物理槽 1");
        bf.set_cell(1, 1, "x2".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(1, 1)[0], vec!["", "x2", "b2"]);
        assert_eq!(physical_len(&bf), 3, "Inserted(0) 位于源区之后的槽 2");
    }

    /// 跨末尾粘贴把既有格变化和新增行最终内容合并为一条记录。
    #[test]
    fn paste_rows_append_and_existing_changes_one_step() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let original = bf.rows(0, 5);
        let out = bf
            .paste_rows(
                4,
                0,
                vec![
                    vec!["Eve2".into(), "23".into(), "SH".into()],
                    vec!["N1".into(), "1".into(), "A".into()],
                    vec!["N2".into(), "2".into()],
                ],
                Approval::deny(),
            )
            .unwrap();
        assert!(out.changed);
        assert_eq!(out.inserted_rows, 2);
        let pasted = bf.rows(0, 7);
        assert_eq!(pasted[4], vec!["Eve2", "23", "SH"]);
        assert_eq!(pasted[5], vec!["N1", "1", "A"]);
        assert_eq!(pasted[6], vec!["N2", "2", ""], "新行短行补空");
        assert_eq!(bf.history_stats().undo_count, 1);
        for _ in 0..2 {
            assert!(bf.undo());
            assert_eq!(bf.rows(0, 5), original);
            assert!(!bf.meta().can_undo, "粘贴只占一步");
            assert!(bf.redo());
            assert_eq!(bf.rows(0, 7), pasted);
        }
    }

    /// 粘贴到末尾的全空行仍是结构变化，也必须一步撤销/重做。
    #[test]
    fn paste_rows_empty_appended_rows_are_real_change() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let out = bf
            .paste_rows(5, 0, vec![Vec::new(), vec![String::new()]], Approval::deny())
            .unwrap();
        assert!(out.changed);
        assert_eq!(out.inserted_rows, 2);
        assert_eq!(bf.rows(0, 7)[5], vec!["", "", ""]);
        assert_eq!(bf.rows(0, 7)[6], vec!["", "", ""]);
        assert!(bf.undo());
        assert_eq!(bf.meta().rows, 5);
        assert!(bf.redo());
        assert_eq!(bf.meta().rows, 7);
    }

    /// 既有区域的全同值/空短行粘贴是空改动，不动 revision 和历史。
    #[test]
    fn paste_rows_noop_does_not_touch_history() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let before = bf.rows(0, 5);
        let out = bf
            .paste_rows(0, 0, vec![vec!["Alice".into()], Vec::new()], Approval::deny())
            .unwrap();
        assert!(!out.changed);
        assert_eq!(out.inserted_rows, 0);
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.history_stats().undo_count == 0 && bf.history_stats().redo_count == 0);
        assert_eq!(bf.revision, 0);
    }

    /// 排序态既有区域粘贴按显示行映射到稳定源记录。
    #[test]
    fn paste_rows_sorted_display_mapping() {
        let p = tmp_csv("paste_sorted", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap(); // e,d,c,b,a
        let out = bf
            .paste_rows(0, 1, vec![vec!["S0".into()], vec!["S1".into()]], Approval::deny())
            .unwrap();
        assert!(out.changed && out.inserted_rows == 0);
        assert_eq!(bf.rows(0, 2)[0], vec!["e", "S0", "foo"]);
        assert_eq!(bf.rows(0, 2)[1], vec!["d", "S1", "Bar"]);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 2)[0][1], "x");
        assert_eq!(bf.rows(0, 2)[1][1], "2");
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 2)[0][1], "S0");
        assert_eq!(bf.rows(0, 2)[1][1], "S1");
        fs::remove_file(&p).ok();
    }

    /// 排序态需要扩行时，在写入最后一个既有显示行之前整次拒绝。
    #[test]
    fn paste_rows_sorted_growth_rejected_atomically() {
        let p = tmp_csv("paste_sorted_growth", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap();
        let before = bf.rows(0, 5);
        let err = bf
            .paste_rows(4, 0, vec![vec!["a2".into()], vec!["NEW".into()]], Approval::deny())
            .unwrap_err();
        assert!(err.contains("排序"), "{err}");
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.history_stats().undo_count == 0 && bf.history_stats().redo_count == 0);
        fs::remove_file(&p).ok();
    }

    /// 渐进索引期可写已发布行，但扩到未发布区域必须原子拒绝。
    #[test]
    fn paste_rows_indexing_growth_rejected_atomically() {
        let mut text = String::from("a,b\r\n");
        for i in 0..20 {
            text.push_str(&format!("{i:04},x\r\n"));
        }
        let p = tmp_csv("paste_indexing", text.as_bytes());
        let mut bf = BigFile::open_progressive_with(&p, 29, 1000, 8, false).unwrap();
        assert!(bf.is_indexing());
        let before = bf.rows(0, 3);
        let err = bf
            .paste_rows(2, 0, vec![vec!["EDIT".into()], vec!["NEW".into()]], Approval::deny())
            .unwrap_err();
        assert!(err.contains("索引"), "{err}");
        assert_eq!(bf.rows(0, 3), before);
        assert!(bf.history_stats().undo_count == 0 && bf.history_stats().redo_count == 0);
        assert_eq!(bf.revision, 0);
        let out = bf.paste_rows(1, 0, vec![vec!["OK".into()]], Approval::deny()).unwrap();
        assert!(out.changed && out.inserted_rows == 0);
        assert_eq!(bf.rows(1, 1)[0][0], "OK");
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 3), before);
        fs::remove_file(&p).ok();
    }

    /// 粘贴位置、列范围和整数加法越界都在任何写入前拒绝。
    #[test]
    fn paste_rows_invalid_position_or_column_rejected_atomically() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let before = bf.rows(0, 5);
        assert!(bf.paste_rows(6, 0, vec![vec!["X".into()]], Approval::deny()).is_err());
        assert!(bf.paste_rows(0, 3, vec![vec!["X".into()]], Approval::deny()).is_err());
        assert!(bf
            .paste_rows(0, u32::MAX, vec![vec!["X".into(), "Y".into()]], Approval::deny())
            .is_err());
        assert!(bf
            .paste_rows(u64::MAX, 0, vec![vec!["X".into()], vec!["Y".into()]], Approval::deny())
            .is_err());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.history_stats().undo_count == 0 && bf.history_stats().redo_count == 0);
        assert_eq!(bf.revision, 0);
    }

    /// 扩行粘贴的插入行也计入完整成本；未确认时不写行或既有格。
    #[test]
    fn paste_rows_snapshot_budget_rejected_atomically() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_history_budget(4_096).unwrap();
        let before = bf.rows(0, 5);
        let revision = bf.revision;
        let outcome = bf
            .paste_rows(
                5,
                0,
                vec![vec!["界".repeat(700_000)]],
                Approval::deny(),
            )
            .unwrap();
        assert!(!outcome.changed);
        assert!(outcome.fixed_point.is_some());
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.meta().rows, 5);
        assert_eq!(bf.history_stats().undo_count, 0);
        assert_eq!(bf.revision, revision);
    }

    /// 扩行粘贴重做换块后，后续新行单格编辑仍落到活块。
    #[test]
    fn paste_rows_redo_rebinds_new_row_cell_edit() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let out = bf
            .paste_rows(5, 0, vec![vec!["N0".into(), "0".into()]], Approval::deny())
            .unwrap();
        assert_eq!(out.inserted_rows, 1);
        bf.set_cell(5, 0, "EDITED".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        assert!(bf.undo());
        assert!(bf.redo());
        assert!(bf.redo());
        assert_eq!(bf.rows(5, 1)[0][0], "EDITED");
    }

    /// 普通插行重做换块后，后续 SetCells 的 Ins 地址重绑、Src 地址不变。
    #[test]
    fn insert_rows_redo_rebinds_following_set_cells() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.insert_rows(
            0,
            Some(vec![vec!["INS".into(), "".into(), "".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.set_cells(vec![(0, 0, "BATCH".into()), (1, 0, "ALICE".into())], Approval::deny())
            .unwrap();
        assert!(bf.undo());
        assert!(bf.undo());
        assert!(bf.redo());
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 1)[0][0], "BATCH");
        assert_eq!(bf.rows(1, 1)[0][0], "ALICE");
    }

    /// 普通插行重做换块后，后续 Paste.changes 也不得引用旧块。
    #[test]
    fn insert_rows_redo_rebinds_following_paste_changes() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.insert_rows(
            0,
            Some(vec![vec!["INS".into(), "".into(), "".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.paste_rows(0, 0, vec![vec!["PASTED".into()]], Approval::deny()).unwrap();
        assert!(bf.undo());
        assert!(bf.undo());
        assert!(bf.redo());
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 1)[0][0], "PASTED");
    }

    /// Paste 逆操作先校验完整新行块；失败时连既有格也不得恢复一半。
    #[test]
    fn paste_rows_undo_validation_failure_avoids_partial_restore() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.paste_rows(4, 0, vec![vec!["Eve2".into()], vec!["NEW".into()]], Approval::deny())
            .unwrap();
        let revision = bf.revision;
        let undo_len = bf.history_stats().undo_count;
        let block = match bf.history.undo_op().unwrap() {
            Op::Paste { inserted: Some(ins), .. } => ins.block,
            other => panic!("应为含插入块的 Paste：{other:?}"),
        };
        bf.seg.blocks[block as usize].as_mut().unwrap()[0][0] = "TAMPER".into();
        assert!(!bf.undo());
        assert_eq!(bf.meta().rows, 6);
        assert_eq!(bf.rows(4, 1)[0][0], "Eve2", "不得只恢复既有格");
        assert_eq!(bf.rows(5, 1)[0][0], "TAMPER");
        assert_eq!(bf.history_stats().undo_count, undo_len);
        assert!(bf.history_stats().redo_count == 0);
        assert_eq!(bf.revision, revision);
    }

    /// 删除撤销若在后续插入行校验失败，前面的源行也不得已恢复。
    #[test]
    fn remove_rows_undo_validation_failure_avoids_partial_restore() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.insert_rows(
            1,
            Some(vec![vec!["TMP".into(), "1".into(), "X".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.remove_rows(vec![0, 1], Approval::deny()).unwrap();
        let before_rows = bf.rows(0, 6);
        let before_stats = bf.history_stats();
        let before_revision = bf.revision;
        let before_generation = bf.history_generation;
        let block = match bf.history.undo_op().unwrap() {
            Op::RemoveRows { items } => items.iter().find_map(|item| match item {
                DeletedRow::Ins { block, .. } => Some(*block),
                DeletedRow::Src { .. } | DeletedRow::Suspended { .. } => None,
            }).unwrap(),
            other => panic!("应为删除行记录：{other:?}"),
        };

        // 空块占用但无对应段不会改变可见行，却会让后续插入行恢复失败。
        bf.seg.blocks[block as usize] = Some(Vec::new());
        assert!(!bf.undo());
        assert_eq!(bf.rows(0, 6), before_rows, "不得只恢复前面的源行");
        assert_eq!(bf.history_stats(), before_stats);
        assert_eq!(bf.revision, before_revision);
        assert_eq!(bf.history_generation, before_generation);
    }

    #[test]
    fn big_insert_remove_undo_restores_order() {
        let p = tmp_csv("insdel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 在第 2 行前插入两行（带值 + 空行）
        bf.insert_rows(2, Some(vec![vec!["NEW".into(), "1".into(), "Z".into()]]), 0, Approval::deny())
            .unwrap();
        bf.insert_rows(3, None, 1, Approval::deny()).unwrap();
        assert_eq!(bf.meta().rows, 7);
        assert_eq!(bf.rows(2, 2)[0][0], "NEW");
        assert_eq!(bf.rows(3, 1)[0], vec!["", "", ""]);
        // 删：一个插入块行 + 一个源行（原第 0 行 Alice 现为 vrow 0）
        assert_eq!(bf.remove_rows(vec![3, 0], Approval::deny()).unwrap().removed_rows, 2);
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

    /// 删除插入行再撤销必须恢复原块，供更早的单元格历史继续寻址。
    #[test]
    fn remove_inserted_row_undo_preserves_older_cell_history() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let original = bf.rows(0, 5);
        bf.insert_rows(
            1,
            Some(vec![vec!["INS".into(), "0".into(), "X".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.set_cell(1, 0, "EDITED".into(), Approval::deny()).unwrap();
        assert_eq!(bf.remove_rows(vec![1], Approval::deny()).unwrap().removed_rows, 1);
        assert_eq!(bf.meta().rows, 5);

        assert!(bf.undo(), "撤销删除");
        assert_eq!(bf.rows(1, 1)[0][0], "EDITED");
        assert!(bf.undo(), "撤销插入行上的旧编辑");
        assert_eq!(bf.rows(1, 1)[0][0], "INS", "旧编辑必须写回原插入块");
        assert!(bf.undo(), "撤销插行");
        assert_eq!(bf.rows(0, 5), original);

        assert!(bf.redo(), "重做插行");
        assert!(bf.redo(), "重做编辑");
        assert_eq!(bf.rows(1, 1)[0][0], "EDITED");
        assert!(bf.redo(), "重做删除");
        assert_eq!(bf.rows(0, 5), original);
    }

    /// 内部段表异常在部分删除后暴露时，整批必须报错并保持原状态。
    #[test]
    fn remove_rows_internal_failure_restores_data_and_history() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.insert_rows(
            1,
            Some(vec![vec!["FIRST".into(), "1".into(), "X".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.insert_rows(
            2,
            Some(vec![vec!["SECOND".into(), "2".into(), "Y".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        bf.set_cell(0, 0, "TEMP".into(), Approval::deny()).unwrap();
        assert!(bf.undo(), "保留一条 redo 用于失败原子性断言");

        // 两个 Ins 段异常共享单块：高行删除会摘块，使随后的低行删除失败。
        let block = match bf.seg.segs[1].kind {
            SegKind::Ins { block } => block,
            other => panic!("首段应为插入块：{other:?}"),
        };
        bf.seg.segs[2].kind = SegKind::Ins { block };
        let before_rows = bf.rows(0, 7);
        assert_eq!(before_rows[1][0], "FIRST");
        assert_eq!(before_rows[2][0], "FIRST", "故障应使两个段共享同一块");
        let before_stats = bf.history_stats();
        let before_revision = bf.revision;
        let before_generation = bf.history_generation;
        let before_modified = bf.modified();
        let before_redo = bf.history.redo_op().cloned();

        let err = bf
            .remove_rows(vec![1, 2], Approval::deny())
            .expect_err("部分删除后的内部失败不得按固定点成功");
        assert!(err.contains("内部"), "应明确报告内部一致性错误：{err}");
        assert_eq!(bf.rows(0, 7), before_rows, "失败后必须恢复原数据");
        assert_eq!(bf.history_stats(), before_stats, "历史账目不得变化");
        assert_eq!(bf.history.redo_op(), before_redo.as_ref(), "redo 不得清空");
        assert_eq!(bf.revision, before_revision);
        assert_eq!(bf.history_generation, before_generation);
        assert_eq!(bf.modified(), before_modified);
    }

    /// 删除源行必须在任何保存发生前保留完整内容供撤销恢复。
    #[test]
    fn deleted_source_row_has_content_before_any_save() {
        let p = tmp_csv("p3_snapshot", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.remove_rows(vec![4], Approval::deny()).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), before[..4]);
        fs::remove_file(&p).ok();
    }

    /// 后备代际变化后，撤销删除必须从自包含快照恢复原内容。
    #[test]
    fn deleted_source_row_undo_uses_snapshot_after_backing_change() {
        let p = tmp_csv("p3_snapshot_backing", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        let rec = match bf.seg.resolve(4) {
            Some(ResolvedRow::Src(rec)) => rec,
            other => panic!("末行应解析为源记录：{other:?}"),
        };
        bf.remove_rows(vec![4], Approval::deny()).unwrap();
        let (start, end) = bf.src_span(rec).unwrap();
        let mut replacement = bf.bytes().to_vec();
        for byte in &mut replacement[start..end] {
            if byte.is_ascii_alphanumeric() {
                *byte = b'X';
            }
        }
        bf.backing = Backing::Ram(replacement);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn bigfile_spool_and_backing_path_are_session_owned() {
        let p = tmp_csv("p3_owned_spool", &people_csv());
        let disk = BigFile::open(&p).unwrap();
        assert_eq!(disk.backing_path, p);
        let memory = BigFile::from_bytes(people_csv(), false).unwrap();
        assert_eq!(memory.backing_path, PathBuf::new());
        let disk_root = disk.spool.root().to_path_buf();
        let memory_root = memory.spool.root().to_path_buf();
        assert_ne!(disk_root, memory_root);
        assert!(disk_root.exists());
        assert!(memory_root.exists());
        drop(disk);
        assert!(!disk_root.exists());
        assert!(memory_root.exists());
        drop(memory);
        assert!(!memory_root.exists());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_remove_redo_descending_safe() {
        let p = tmp_csv("redodel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 连续删前两行再重做：升序删除会因位移错位，引擎须内部降序
        bf.remove_rows(vec![0, 1], Approval::deny()).unwrap();
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
        bf.insert_rows(1, Some(vec![vec!["TMP".into(), "0".into(), "X".into()]]), 0, Approval::deny())
            .unwrap();
        // 删源行(0) + 删插入行(1)，undo 后 redo：两类行都必须重新消失
        bf.remove_rows(vec![0, 1], Approval::deny()).unwrap();
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
    fn big_filter_page_clamps_out_of_range_start() {
        let p = tmp_csv("filter_page_oor", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.run_filter(&FilterSpec {
            cols: vec![(2, "beijing".into(), 0)],
            multi_cols: vec![],
            multi_text: String::new(),
        })
        .unwrap();
        assert_eq!(bf.filter_len(), 2);
        // 快速拖动后迟到的旧页请求会带着筛选前的行号（如 19000）进来：
        // 起点越界必须返回空页，不能切片 panic（WebView2 进程直接崩溃）。
        assert!(bf.filter_page(19000, 200).rows.is_empty());
        // 部分越界：只返回剩余命中
        assert_eq!(bf.filter_page(1, 200).rows.len(), 1);
        fs::remove_file(&p).ok();
    }

    /// 筛选分页附带原始行号：未排序时 vrows 即命中 vrow，且与 rows 逐行对齐。
    #[test]
    fn filter_page_returns_orig_vrows() {
        let p = tmp_csv("filter_page_vrows", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.run_filter(&spec_one(2, "beijing")).unwrap(); // Alice(0)、Carol(2)
        let page = bf.filter_page(0, 200);
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.vrows, vec![0, 2], "命中行的原始行号");
        assert_eq!(page.rows[0][0], "Alice");
        assert_eq!(page.rows[1][0], "Carol");
        assert_eq!(page.rows.len(), page.vrows.len(), "rows/vrows 必须等长对齐");
        fs::remove_file(&p).ok();
    }

    /// 排序+筛选共存：vrows 必须经 orig_vrow 换算回原始行号（命中集存显示位置）。
    #[test]
    fn filter_page_vrows_sorted_mapped_to_orig() {
        let p = tmp_csv("filter_page_vrows_sort", &sort_fixture());
        let mut bf = load_ram(&p);
        assert_eq!(bf.run_filter(&spec_one(2, "foo")).unwrap(), 3); // a,c,e
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap(); // id 降序 e,d,c,b,a → 命中显示位置 e=0,c=2,a=4
        let page = bf.filter_page(0, 3);
        assert_eq!(
            page.rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
            vec!["e", "c", "a"]
        );
        assert_eq!(page.vrows, vec![4, 2, 0], "行号应为原始 vrow 而非显示位置");
        fs::remove_file(&p).ok();
    }

    /// 命中序号 → 清筛后全量视图行号（取消筛选锚定用）：边界与排序态。
    #[test]
    fn filter_hit_view_row_bounds_and_sorted() {
        let p = tmp_csv("filter_hit_view_row", &sort_fixture());
        let mut bf = load_ram(&p);
        assert_eq!(bf.filter_hit_view_row(0), None, "无筛选态恒 None");
        assert_eq!(bf.run_filter(&spec_one(2, "foo")).unwrap(), 3); // 未排序命中 [0,2,4]
        assert_eq!(bf.filter_hit_view_row(0), Some(0));
        assert_eq!(bf.filter_hit_view_row(2), Some(4));
        assert_eq!(bf.filter_hit_view_row(3), None, "越界 None");
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap(); // 命中集换算为显示位置 [0,2,4]（e,c,a）
        assert_eq!(
            bf.filter_hit_view_row(1),
            Some(2),
            "返回显示位置原值（= 清筛后全量视图行号）"
        );
        bf.clear_filter();
        assert_eq!(bf.filter_hit_view_row(0), None, "清筛后 None");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_filter_and_multi_or_semantics() {
        let p = tmp_csv("filter", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 单列 AND：city 含 "beijing"（小写匹配）
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(2, "beijing".into(), 0)],
                multi_cols: vec![],
                multi_text: String::new(),
            })
            .unwrap();
        assert_eq!(n, 2); // Alice + Carol
        // 双列 AND：name 含 "a" 且 city 含 "beijing" → Alice + Carol
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(0, "a".into(), 0), (2, "beijing".into(), 0)],
                multi_cols: vec![],
                multi_text: String::new(),
            })
            .unwrap();
        assert_eq!(n, 2);
        // 双列 AND 无交集 → 0
        let n = bf
            .run_filter(&FilterSpec {
                cols: vec![(0, "a".into(), 0), (2, "shanghai".into(), 0)],
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
        let page = bf.filter_page(0, 10).rows;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0][0], "Carol");
        assert!(bf.has_filter());
        // 筛选结果对源行编辑立即生效（覆盖层参与）
        bf.set_cell(2, 0, "carolina".into(), Approval::deny()).unwrap();
        let page = bf.filter_page(0, 10).rows;
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
    fn p1_clear_structured_targets_roundtrip() {
        let p = tmp_csv("p1_clear_targets", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let targets = [
            ClearTarget::Rows { rows: vec![1] },
            ClearTarget::Columns { cols: vec![1] },
            ClearTarget::Rect { row: 0, col: 0, rows: 2, cols: 2 },
        ];
        for target in targets {
            let before = bf.rows(0, u32::try_from(bf.meta().rows).unwrap());
            assert!(bf.clear(target, Approval::deny()).unwrap().changed);
            let changed = bf.rows(0, u32::try_from(bf.meta().rows).unwrap());
            assert_ne!(changed, before);
            assert!(bf.undo());
            assert_eq!(bf.rows(0, u32::try_from(bf.meta().rows).unwrap()), before);
            assert!(bf.redo());
            assert_eq!(bf.rows(0, u32::try_from(bf.meta().rows).unwrap()), changed);
        }
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_clear_over_budget_preserves_data_and_redo() {
        let p = tmp_csv("p1_clear_fixed", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        let before = bf.rows(0, u32::try_from(bf.meta().rows).unwrap());
        let history = bf.history_stats();
        assert_eq!(history.redo_count, 1);
        bf.set_history_budget(history.bytes).unwrap();
        let history = bf.history_stats();
        let first = bf.clear(
            ClearTarget::Columns { cols: vec![0, 1, 2] },
            Approval::deny(),
        ).unwrap();
        assert!(!first.changed);
        assert!(first.fixed_point.is_some());
        assert_eq!(bf.rows(0, u32::try_from(bf.meta().rows).unwrap()), before);
        assert_eq!(bf.history_stats(), history);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_clear_sparse_cells_roundtrip() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let before = bf.rows(0, 5);
        let outcome = bf.clear(
            ClearTarget::Cells { cells: vec![(1, 0), (3, 2), (1, 0)] },
            Approval::deny(),
        ).unwrap();
        assert!(outcome.changed);
        assert_eq!(bf.history_stats().undo_count, 1);
        let changed = bf.rows(0, 5);
        assert_eq!(changed[1][0], "");
        assert_eq!(changed[3][2], "");
        assert_eq!(changed[1][1], before[1][1]);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), changed);
    }

    #[test]
    fn p1_clear_confirmed_fixed_point_streams_and_bounds_later_undo() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        let history = bf.history_stats();
        bf.set_history_budget(history.bytes).unwrap();
        let request = bf.clear(
            ClearTarget::Columns { cols: vec![0, 1, 2] },
            Approval::deny(),
        ).unwrap().fixed_point.unwrap();
        let done = bf.clear(
            ClearTarget::Columns { cols: vec![0, 1, 2] },
            Approval::allow(request.preflight_id),
        ).unwrap();
        assert!(done.changed);
        assert!(done.fixed_point.is_none());
        assert_eq!(bf.history_stats().undo_count, 0);
        assert_eq!(bf.history_stats().redo_count, 0);
        assert!(bf.rows(0, 5).iter().flatten().all(String::is_empty));
        bf.set_cell(0, 0, "after".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 1)[0][0], "");
        assert!(!bf.undo(), "固定点之前的整列清除不得撤销");
    }

    #[test]
    fn p1_clear_invalid_rect_and_empty_target_are_atomic() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 0, "redo-base".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        let before = bf.rows(0, 5);
        let history = bf.history_stats();
        let err = bf.clear(
            ClearTarget::Rect { row: 4, col: 0, rows: 2, cols: 1 },
            Approval::deny(),
        ).unwrap_err();
        assert!(err.contains("行号越界"));
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.history_stats(), history);
        let err = bf.clear(
            ClearTarget::Rect { row: u64::MAX, col: 0, rows: 2, cols: 1 },
            Approval::deny(),
        ).unwrap_err();
        assert!(err.contains("行号越界"));
        assert_eq!(bf.history_stats(), history);
        let noop = bf.clear(
            ClearTarget::Rect { row: 5, col: 3, rows: 0, cols: 0 },
            Approval::deny(),
        ).unwrap();
        assert!(!noop.changed);
        assert_eq!(bf.history_stats(), history, "空矩形不得清 redo 或改代际");
    }

    #[test]
    fn p1_fixed_point_requires_confirmation_and_bounds_later_undo() {
        let p = tmp_csv("p1_fixed_point", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_history_budget(4_096).unwrap();
        let large = "x".repeat(128 * 1024);
        let first = bf.set_cell(0, 0, large.clone(), Approval::deny()).unwrap();
        assert!(!first.changed);
        let request = first.fixed_point.unwrap();
        assert_eq!(request.budget_bytes, 4_096);
        assert!(request.required_bytes > 4_096);
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        let done = bf.set_cell(0, 0, large.clone(), Approval::allow(request.preflight_id)).unwrap();
        assert!(done.changed);
        assert_eq!(bf.history_stats().undo_count, 0);
        assert!(bf.modified());
        bf.set_cell(1, 0, "after".into(), Approval::deny()).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 1)[0][0], large);
        assert!(!bf.undo());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_fixed_point_preflight_expires_after_another_edit() {
        let p = tmp_csv("p1_preflight", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_history_budget(4_096).unwrap();
        let large = "x".repeat(128 * 1024);
        let request = bf.set_cell(0, 0, large.clone(), Approval::deny())
            .unwrap().fixed_point.unwrap();
        bf.set_cell(1, 0, "newer".into(), Approval::deny()).unwrap();
        let err = bf.set_cell(0, 0, large, Approval::allow(request.preflight_id)).unwrap_err();
        assert!(err.contains("预检已过期"));
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_undo_before_savepoint_is_dirty_and_redo_returns_clean() {
        let p = tmp_csv("p1_signed_revision", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "changed".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(!bf.modified());
        assert!(bf.undo());
        assert!(bf.modified());
        assert!(bf.redo());
        assert!(!bf.modified());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_branch_after_undo_past_savepoint_stays_dirty() {
        let p = tmp_csv("p1_detached_savepoint", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        bf.set_cell(0, 0, "branch".into(), Approval::deny()).unwrap();
        assert!(bf.modified());
        assert!(bf.undo());
        assert!(bf.modified());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_fixed_point_after_undo_past_savepoint_stays_dirty_at_zero_revision() {
        let p = tmp_csv("p1_fixed_detached", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        bf.set_history_budget(4_096).unwrap();
        let large = "x".repeat(128 * 1024);
        let request = bf.set_cell(1, 0, large.clone(), Approval::deny())
            .unwrap().fixed_point.unwrap();
        let done = bf.set_cell(1, 0, large, Approval::allow(request.preflight_id)).unwrap();
        assert!(done.changed);
        assert!(bf.modified(), "保存点已在被清除的 redo 分支上，revision=0 也不能假干净");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_preflight_rejects_stale_allow_when_step_now_fits() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_history_budget(4_096).unwrap();
        let large = "x".repeat(128 * 1024);
        let request = bf
            .set_cell(0, 0, large.clone(), Approval::deny())
            .unwrap()
            .fixed_point
            .unwrap();
        bf.set_history_budget(1024 * 1024).unwrap();
        let err = bf
            .set_cell(0, 0, large, Approval::allow(request.preflight_id))
            .unwrap_err();
        assert!(err.contains("预检已过期"));
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        assert_eq!(bf.history_stats().undo_count, 0);
    }

    #[test]
    fn p1_preflight_revision_overflow_is_atomic() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let before = bf.rows(0, 5);
        bf.revision = i64::MAX;
        let err = bf.set_cell(0, 0, "overflow".into(), Approval::deny()).unwrap_err();
        assert!(err.contains("修订号溢出"));
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.revision, i64::MAX);
        assert_eq!(bf.history_stats().undo_count, 0);
    }

    #[test]
    fn p1_preflight_generation_overflow_is_atomic() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let before = bf.rows(0, 5);
        bf.history_generation = u64::MAX;
        let err = bf.set_cell(0, 0, "overflow".into(), Approval::deny()).unwrap_err();
        assert!(err.contains("历史代际溢出"));
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.history_generation, u64::MAX);
        assert_eq!(bf.history_stats().undo_count, 0);
    }

    /// 缩容固定点切断保存点后的 redo 时，revision 回到 0 仍须保持脏。
    #[test]
    fn p1_budget_fixed_point_detaches_unreachable_savepoint() {
        let p = tmp_csv("p1_budget_detached", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "saved".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        let change = bf.set_history_budget(1).unwrap();
        assert!(change.fixed_point);
        assert!(bf.modified());
        bf.set_history_budget(4_096).unwrap();
        bf.set_cell(0, 0, "branch".into(), Approval::deny()).unwrap();
        assert!(bf.modified(), "revision=0 也不能回到已不可达的保存点");
        assert!(bf.undo());
        assert!(bf.modified());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p1_small_operations_are_not_capped_at_200() {
        let p = tmp_csv("p1_no_step_cap", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        for i in 0..201 {
            bf.set_cell(0, 0, i.to_string(), Approval::deny()).unwrap();
        }
        assert_eq!(bf.history_stats().undo_count, 201);
        assert!(bf.history_stats().bytes <= DEFAULT_HISTORY_BUDGET_BYTES);
        fs::remove_file(&p).ok();
    }


    #[test]
    fn p8_save_migrates_source_values_to_shared_text() {
        let p = tmp_csv("p8_save_value_refs", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "Changed".into(), Approval::deny()).unwrap();
        let before_bytes = bf.history_stats().bytes;
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        assert_eq!(bf.history_stats().bytes, before_bytes);
        let Some(Op::SetCell { old, new, .. }) = bf.history.undo_op() else {
            panic!("保存后必须保留 SetCell 历史");
        };
        assert!(matches!(old, ValueRef::Text(text) if text.text.as_ref() == "Alice"));
        assert!(matches!(new, ValueRef::Text(text) if text.text.as_ref() == "Changed"));
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 1)[0][0], "Changed");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_insert_then_undo_removes_materialized_row() {
        let p = tmp_csv("p8_save_insert", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        let inserted = vec![vec!["X".to_string(), "Y".to_string(), "Z".to_string()]];
        bf.insert_rows(1, Some(inserted.clone()), 1, Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1), inserted);
        fs::remove_file(&p).ok();
    }

    /// 保存后交错撤销插入、删除和更早源格编辑，身份必须按顺序重绑定。
    #[test]
    fn p3_save_interleaved_delete_cell_history_rebinds_in_order() {
        let p = tmp_csv("p3_interleaved", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.set_cell(4, 0, "last-before-delete".into(), Approval::deny()).unwrap();
        bf.remove_rows(vec![4], Approval::deny()).unwrap();
        bf.insert_rows(1, Some(vec![vec!["X".into(), "Y".into(), "Z".into()]]), 1,
            Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        assert!(bf.undo());
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert!(bf.redo());
        assert!(bf.redo());
        fs::remove_file(&p).ok();
    }


    #[test]
    fn p3_save_delete_middle_undo_restores_all_original_positions() {
        let p = tmp_csv("p3_save_delete_middle", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.set_cell(2, 0, "Carol-before-delete".into(), Approval::deny()).unwrap();
        bf.remove_rows(vec![2], Approval::deny()).unwrap();
        let deleted = bf.rows(0, 4);
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();

        assert!(bf.undo());
        let mut restored = before.clone();
        restored[2][0] = "Carol-before-delete".into();
        assert_eq!(bf.rows(0, 5), restored);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), restored);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 4), deleted);
        fs::remove_file(&p).ok();
    }


    #[test]
    fn p8_save_insert_edit_rebinds_materialized_history() {
        let p = tmp_csv("p8_save_insert_edit", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.insert_rows(1, Some(vec![vec!["X".into(), "Y".into(), "Z".into()]]), 1,
            Approval::deny()).unwrap();
        bf.set_cell(1, 0, "Edited".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(1, 1)[0][0], "X");
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1)[0][0], "X");
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1)[0][0], "Edited");
        assert!(bf.undo());
        assert_eq!(bf.rows(1, 1)[0][0], "X");
        assert!(bf.redo());
        assert_eq!(bf.rows(1, 1)[0][0], "Edited");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_repeat_save_preserves_suspended_deleted_identity() {
        let p = tmp_csv("p8_suspended_collision", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.set_cell(3, 0, "dave-edited".into(), Approval::deny()).unwrap();
        bf.set_cell(2, 0, "Carol-edited".into(), Approval::deny()).unwrap();
        bf.remove_rows(vec![2], Approval::deny()).unwrap();
        let deleted = bf.rows(0, 4);
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);

        assert!(bf.undo());
        let mut restored = before.clone();
        restored[2][0] = "Carol-edited".into();
        restored[3][0] = "dave-edited".into();
        assert_eq!(bf.rows(0, 5), restored);
        assert!(bf.undo());
        restored[2][0] = "Carol".into();
        assert_eq!(bf.rows(0, 5), restored);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert!(bf.redo());
        assert!(bf.redo());
        let mut deleted_edited = deleted;
        deleted_edited[2][0] = "dave-edited".into();
        assert_eq!(bf.rows(0, 4), deleted_edited);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_deleted_inserted_row_restores_original_block() {
        let p = tmp_csv("p8_deleted_inserted", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        let inserted = vec!["X".to_string(), "Y".to_string(), "Z".to_string()];
        bf.insert_rows(1, Some(vec![inserted.clone()]), 1, Approval::deny()).unwrap();
        bf.remove_rows(vec![1], Approval::deny()).unwrap();
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        assert!(bf.undo());
        assert_eq!(bf.rows(1, 1)[0], inserted);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), before);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_after_undo_delete_preserves_redo_identity() {
        let p = tmp_csv("p8_suspended_redo", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.remove_rows(vec![2], Approval::deny()).unwrap();
        let deleted = bf.rows(0, 4);
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 4), deleted);
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 4), deleted);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_suspended_restore_failure_is_atomic() {
        let p = tmp_csv("p8_suspended_atomic", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.insert_rows(
            1,
            Some(vec![vec!["TMP".into(), "1".into(), "X".into()]]),
            0,
            Approval::deny(),
        ).unwrap();
        let inserted_state = bf.rows(0, 6);
        bf.remove_rows(vec![0, 1, 3], Approval::deny()).unwrap();
        let deleted_state = bf.rows(0, 3);
        assert!(bf.save(Approval::deny(), |_, _, _, _| {}).unwrap().saved);
        let before_stats = bf.history_stats();
        let before_revision = bf.revision;
        let before_generation = bf.history_generation;
        let block = match bf.history.undo_op().unwrap() {
            Op::RemoveRows { items } => items.iter().find_map(|item| match item {
                DeletedRow::Ins { block, .. } => Some(*block),
                DeletedRow::Src { .. } | DeletedRow::Suspended { .. } => None,
            }).unwrap(),
            other => panic!("应为删除行记录：{other:?}"),
        };
        bf.seg.blocks[block as usize] = Some(Vec::new());
        assert!(!bf.undo());
        assert_eq!(bf.rows(0, 3), deleted_state);
        assert_eq!(bf.history_stats(), before_stats);
        assert_eq!(bf.revision, before_revision);
        assert_eq!(bf.history_generation, before_generation);
        bf.seg.blocks[block as usize] = None;
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 6), inserted_state);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 3), deleted_state);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_temp_validation_failure_keeps_file_and_history() {
        let p = tmp_csv("p8_temp_validation", &people_csv());
        let original = fs::read(&p).unwrap();
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        let before = bf.rows(0, 5);
        let before_stats = bf.history_stats();
        let before_revision = bf.revision;
        let tmp = tmp_path_for(&p);
        let mut corrupted = false;
        let err = bf.save(Approval::deny(), |_, _, phase, _| {
            if phase == PHASE_FLUSH && !corrupted {
                corrupted = true;
                fs::write(&tmp, b"").unwrap();
            }
        }).unwrap_err();
        assert!(corrupted);
        assert!(err.contains("行数校验"), "错误应来自临时重开校验：{err}");
        assert_eq!(fs::read(&p).unwrap(), original, "校验失败不得替换目标");
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.history_stats(), before_stats);
        assert_eq!(bf.revision, before_revision);
        assert_eq!(bf.path, p);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_as_temp_validation_failure_keeps_paths() {
        let p = tmp_csv("p8_temp_validation_source", &people_csv());
        let target = tmp_csv("p8_temp_validation_target", b"h1,h2,h3
keep,old,target
");
        let source_bytes = fs::read(&p).unwrap();
        let target_bytes = fs::read(&target).unwrap();
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        let before = bf.rows(0, 5);
        let before_stats = bf.history_stats();
        let tmp = tmp_path_for(&target);
        let mut corrupted = false;
        let err = bf.save_as(&target, Approval::deny(), |_, _, phase, _| {
            if phase == PHASE_FLUSH && !corrupted {
                corrupted = true;
                fs::write(&tmp, b"").unwrap();
            }
        }).unwrap_err();
        assert!(corrupted);
        assert!(err.contains("行数校验"), "错误应来自临时重开校验：{err}");
        assert_eq!(fs::read(&p).unwrap(), source_bytes);
        assert_eq!(fs::read(&target).unwrap(), target_bytes);
        assert_eq!(bf.rows(0, 5), before);
        assert_eq!(bf.history_stats(), before_stats);
        assert_eq!(bf.path, p);
        fs::remove_file(&p).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn p8_save_delete_last_then_undo_restores_row() {
        let p = tmp_csv("p8_save_delete", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        assert_eq!(bf.remove_rows(vec![4], Approval::deny()).unwrap().removed_rows, 1);
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(0, 5), before);
        assert!(bf.redo());
        assert_eq!(bf.rows(0, 5), before[..4]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn p8_save_undo_source_edit_after_insert_targets_original_row() {
        let p = tmp_csv("p8_save_source_edit", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let before = bf.rows(0, 5);
        bf.insert_rows(
            1,
            Some(vec![vec!["X".to_string(), "Y".to_string(), "Z".to_string()]]),
            1,
            Approval::deny(),
        )
        .unwrap();
        bf.set_cell(2, 0, "changed".to_string(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.undo());
        assert_eq!(bf.rows(1, 1)[0][0], "X");
        assert_eq!(bf.rows(2, 1)[0][0], before[1][0]);
        assert!(bf.redo());
        assert_eq!(bf.rows(2, 1)[0][0], "changed");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_roundtrip_and_keeps_undo() {
        let p = tmp_csv("save", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap(); // Alice age 30→31
        bf.insert_rows(1, Some(vec![vec!["Zed".into(), "1".into(), "X".into()]]), 0, Approval::deny())
            .unwrap();
        bf.remove_rows(vec![3], Approval::deny()).unwrap(); // 删 Carol（插入位移后 vrow 3）
        let mut prog = 0u32;
        bf.save(Approval::deny(), |done, _total, _phase, _rows| prog = prog.max(done as u32))
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
    fn big_save_as_writes_target_and_switches_session() {
        let p = tmp_csv("saveas_src", &people_csv());
        let orig_bytes = fs::read(&p).unwrap();
        let target = std::env::temp_dir().join(format!(
            "st_engine_saveas_dst_{}.csv",
            std::process::id()
        ));
        fs::remove_file(&target).ok(); // 清掉上次运行残留
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap(); // Alice age 30→31

        bf.save_as(&target, Approval::deny(), |_, _, _, _| {}).unwrap();

        // 另存为绝不能改写原文件；目标文件必须包含当前视图（含未保存编辑）
        assert_eq!(fs::read(&p).unwrap(), orig_bytes, "另存为不得改写原文件");
        let bf_new = BigFile::open(&target).unwrap();
        assert_eq!(bf_new.rows(0, 1)[0], vec!["Alice", "31", "Beijing"]);

        // 会话身份已切到新路径：后续普通保存只更新目标文件
        bf.set_cell(1, 1, "26".into(), Approval::deny()).unwrap(); // bob 25→26
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf_after = BigFile::open(&target).unwrap();
        assert_eq!(bf_after.rows(1, 1)[0][1], "26");
        assert_eq!(fs::read(&p).unwrap(), orig_bytes, "另存为后保存不得碰原文件");

        fs::remove_file(&p).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn big_save_as_failure_keeps_session_and_original() {
        let p = tmp_csv("saveas_fail", &people_csv());
        let orig_bytes = fs::read(&p).unwrap();
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        // 目标目录不存在：创建临时文件即失败，不得动原文件/会话
        let bad = std::env::temp_dir()
            .join(format!("st_engine_saveas_missing_{}", std::process::id()))
            .join("dst.csv");

        let err = bf.save_as(&bad, Approval::deny(), |_, _, _, _| {}).unwrap_err();

        assert!(err.contains("创建临时文件失败"), "错误信息应指出目标不可写：{err}");
        assert_eq!(fs::read(&p).unwrap(), orig_bytes, "失败时原文件不得变化");
        assert_eq!(bf.rows(0, 1)[0][1], "31", "失败时会话编辑不得丢失");
        // 会话仍指向原文件，普通保存照常可用
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0][1], "31");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_save_gbk_reencodes() {
        let (enc, _, _) = encoding_rs::GBK.encode("名字,备注\r\n张三,北京\r\n");
        let p = tmp_csv("gbk", &enc.into_owned());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().encoding, "gbk");
        bf.set_cell(0, 1, "上海".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
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
        bf.remove_rows(vec![2], Approval::deny()).unwrap(); // 删 Carol（中间行）
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
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
        bf.set_cell(1, 1, "41".into(), Approval::deny()).unwrap(); // Bob age 40→41
        bf.remove_rows(vec![0], Approval::deny()).unwrap(); // 删 Alice
        bf.insert_rows(1, Some(vec![vec!["Zed".into(), "1".into()]]), 0, Approval::deny())
            .unwrap(); // vrow 1 插 Zed（Bob 之后）
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
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
        assert!(bf.set_cell(0, 0, "x".into(), Approval::deny()).is_err());
        assert!(bf.run_filter(&FilterSpec {
            cols: vec![(0, "a".into(), 0)],
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
        bf.set_cell(0, 0, "Bob".into(), Approval::deny()).unwrap();
        // 模拟外部程序占用：读共享但拒绝删除共享（FILE_SHARE_READ|WRITE=0x3，
        // 同 Excel/资源管理器预览类句柄），rename 必失败（os error 32）
        let lock = fs::OpenOptions::new().read(true).share_mode(0x3).open(&p).unwrap();
        let err = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap_err();
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
        bf.set_cell(0, 0, "Bob".into(), Approval::deny()).unwrap();
        // 读共享占用 400ms 后释放；rename 重试窗口（5 次 × 200ms）应覆盖到
        let lock = fs::OpenOptions::new().read(true).share_mode(0x3).open(&p).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            drop(lock);
        });
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        t.join().unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0][0], "Bob", "新文件应落盘生效");
        fs::remove_file(&p).ok();
    }

    // ---- Part 4：大文件秒筛（字节直扫 + 收窄 + 中断） ----

    fn spec_one(col: u32, kw: &str) -> FilterSpec {
        spec_mode(col, kw, 0)
    }

    /// 单列三态条件：mode 0 包含 / 1 严格等于·不分大小写 / 2 严格等于·区分大小写。
    fn spec_mode(col: u32, kw: &str, mode: u8) -> FilterSpec {
        FilterSpec {
            cols: vec![(col, kw.into(), mode)],
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

        let page = bf.filter_page(0, 10).rows;
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

    // ---- Part 4b：FieldScan 特征化（钉住标量语义，SIMD 快路径必须逐位一致） ----

    /// 直调 FieldScan 扫一条记录（特征化/对拍共用；ncols 给足 32 覆盖超界列）。
    fn scan_one(spec: &FilterSpec, enc: Encoding, rec: &[u8]) -> bool {
        let overlay: HashMap<(u32, ColumnId), String> = HashMap::new();
        let mut fs = FieldScan::new(spec, enc, &overlay, 32);
        fs.record_matches(0, rec)
    }

    /// 字段边界 / 空字段 / 超界列 / 孤 \r / 无尾换行 / 大小写折叠 / 多列 AND。
    #[test]
    fn field_scan_char_boundaries_and_case() {
        let u = Encoding::Utf8;
        // 基础三列
        assert!(scan_one(&spec_one(1, "b"), u, b"a,b,c\r\n"));
        assert!(!scan_one(&spec_one(1, "x"), u, b"a,b,c\r\n"));
        assert!(scan_one(&spec_one(0, "a"), u, b"a,b,c\r\n"));
        assert!(scan_one(&spec_one(2, "c"), u, b"a,b,c\r\n"));
        // 超界列：字段不存在，needle 永不命中
        assert!(!scan_one(&spec_one(3, "c"), u, b"a,b,c\r\n"));
        assert!(!scan_one(&spec_one(9, "x"), u, b"a,b,c\r\n"));
        // 空字段不中非空关键字；邻列不受影响
        assert!(scan_one(&spec_one(0, "a"), u, b"a,,c\r\n"));
        assert!(!scan_one(&spec_one(1, "x"), u, b"a,,c\r\n"));
        assert!(scan_one(&spec_one(2, "c"), u, b"a,,c\r\n"));
        // 尾逗号（末尾空字段）
        assert!(scan_one(&spec_one(1, "b"), u, b"a,b,\r\n"));
        assert!(!scan_one(&spec_one(2, "x"), u, b"a,b,\r\n"));
        // 无尾换行与 LF 结尾
        assert!(scan_one(&spec_one(1, "b"), u, b"a,b"));
        assert!(scan_one(&spec_one(1, "b"), u, b"a,b\n"));
        // 孤 \r 即记录结束，其后字节不属于本记录
        assert!(scan_one(&spec_one(1, "b"), u, b"a,b\rc"));
        assert!(!scan_one(&spec_one(2, "c"), u, b"a,b\rc"));
        // 大小写折叠（ASCII）
        assert!(scan_one(&spec_one(0, "alice"), u, b"ALICE,BeiJing\r\n"));
        assert!(scan_one(&spec_one(1, "beijing"), u, b"ALICE,BeiJing\r\n"));
        // 多列 AND：两列都中才中；前列不中则整行不中（提前退出结果不变式）
        let and2 = FilterSpec {
            cols: vec![(2, "c2".into(), 0), (6, "c6".into(), 0)],
            multi_cols: vec![],
            multi_text: String::new(),
        };
        assert!(scan_one(&and2, u, b"c0,c1,c2,c3,c4,c5,c6,c7\r\n"));
        let and2miss = FilterSpec {
            cols: vec![(2, "nope".into(), 0), (6, "c6".into(), 0)],
            multi_cols: vec![],
            multi_text: String::new(),
        };
        assert!(!scan_one(&and2miss, u, b"c0,c1,c2,c3,c4,c5,c6,c7\r\n"));
    }

    /// 引号全形态：整字段带引、引号内界符、转义对、奇偶连引、未闭合、中段裸引号。
    #[test]
    fn field_scan_char_quotes() {
        let u = Encoding::Utf8;
        // 引号内逗号是内容不是界符
        assert!(scan_one(&spec_one(0, "hello, world"), u, b"\"hello, world\",x\r\n"));
        assert!(scan_one(&spec_one(1, "x"), u, b"\"hello, world\",x\r\n"));
        // 引号内换行属于同一字段
        assert!(scan_one(&spec_one(1, "line2"), u, b"a,\"line1\nline2\",c\r\n"));
        assert!(scan_one(&spec_one(2, "c"), u, b"a,\"line1\nline2\",c\r\n"));
        // "" 转义 → 一个字面引号
        assert!(scan_one(&spec_one(0, "say \"hi\""), u, b"\"say \"\"hi\"\"\",x\r\n"));
        // 四连引 = 开 + 转义对 + 关：内容恰一个引号，后续逗号仍是界符
        assert!(scan_one(&spec_one(0, "\""), u, b"\"\"\"\",x\r\n"));
        assert!(scan_one(&spec_one(1, "x"), u, b"\"\"\"\",x\r\n"));
        // 六连引 = 开 + 两个转义对 + 关：内容两个引号
        assert!(scan_one(&spec_one(0, "\"\""), u, b"\"\"\"\"\"\",x\r\n"));
        assert!(scan_one(&spec_one(1, "x"), u, b"\"\"\"\"\"\",x\r\n"));
        // 字段起点的 "" = 空引用段（开 + 关），内容为空
        assert!(scan_one(&spec_one(1, "x"), u, b"\"\",x\r\n"));
        assert!(!scan_one(&spec_one(0, "\""), u, b"\"\",x\r\n"));
        // 中段裸引号成对：剥离后拼接（ab"cd"ef → abcdef）
        assert!(scan_one(&spec_one(0, "abcdef"), u, b"ab\"cd\"ef,x\r\n"));
        assert!(!scan_one(&spec_one(0, "ab\"cd"), u, b"ab\"cd\"ef,x\r\n"));
        // 三连引后接逗号：转义对发生在引号态内 → 引号未关，逗号是内容
        assert!(scan_one(&spec_one(0, "\",x"), u, b"\"\"\",x"));
        assert!(!scan_one(&spec_one(1, "x"), u, b"\"\"\",x"));
        // 五连引：开 + 两个转义对，未闭合 → 吞掉后续全部字节（含逗号与换行）
        assert!(scan_one(&spec_one(0, "\"\",x"), u, b"\"\"\"\"\",x\r\n"));
        assert!(!scan_one(&spec_one(1, "x"), u, b"\"\"\"\"\",x\r\n"));
        // 未闭合引号：吞到记录尾，col1 不存在
        assert!(scan_one(&spec_one(0, "abc,def"), u, b"\"abc,def\r\n"));
        assert!(!scan_one(&spec_one(1, "def"), u, b"\"abc,def\r\n"));
    }

    /// UTF-8 CJK / GBK 双字节 / 覆盖层优先 / 多重 OR 及其与 AND 的组合。
    #[test]
    fn field_scan_char_encoding_overlay_multi() {
        let u = Encoding::Utf8;
        // UTF-8 CJK 关键字；CJK 记录里的 ASCII 折叠
        assert!(scan_one(&spec_one(0, "北京"), u, "北京,备ABC\r\n".as_bytes()));
        assert!(scan_one(&spec_one(1, "备"), u, "北京,备ABC\r\n".as_bytes()));
        assert!(scan_one(&spec_one(1, "abc"), u, "北京,备ABC\r\n".as_bytes()));
        // GBK 纯 ASCII 记录：与 UTF-8 结果一致
        assert!(scan_one(&spec_one(0, "a"), Encoding::Gbk, b"a,b\r\n"));
        assert!(!scan_one(&spec_one(1, "x"), Encoding::Gbk, b"a,b\r\n"));
        // GBK 双字节记录：引导字节吞次字节，界符落在整对之后
        let gbk = encoding_rs::GBK.encode("名字,值\r\n").0.into_owned();
        assert!(scan_one(&spec_one(0, "名字"), Encoding::Gbk, &gbk));
        assert!(scan_one(&spec_one(1, "值"), Encoding::Gbk, &gbk));
        assert!(!scan_one(&spec_one(1, "名"), Encoding::Gbk, &gbk));
        // 覆盖层优先于原始字节（同列同 rec）
        let mut ov: HashMap<(u32, ColumnId), String> = HashMap::new();
        ov.insert((0, ColumnId(0)), "zzz".to_string());
        let mut fs = FieldScan::new(&spec_one(0, "zzz"), u, &ov, 32);
        assert!(fs.record_matches(0, b"a,b\r\n"));
        let mut fs = FieldScan::new(&spec_one(0, "a"), u, &ov, 32);
        assert!(!fs.record_matches(0, b"a,b\r\n"));
        // 多重 OR：任一勾选列命中即中
        assert!(scan_one(&spec_multi(vec![0, 2], "x"), u, b"a,b,x\r\n"));
        assert!(scan_one(&spec_multi(vec![0, 2], "x"), u, b"x,b,c\r\n"));
        assert!(!scan_one(&spec_multi(vec![0, 2], "x"), u, b"a,b,c\r\n"));
        // AND + OR 组合：逐列 AND 全部命中且 OR 至少中一列
        let combo = FilterSpec {
            cols: vec![(0, "a".into(), 0)],
            multi_cols: vec![1, 2],
            multi_text: "b".into(),
        };
        assert!(scan_one(&combo, u, b"a,x,b\r\n"));
        assert!(!scan_one(&combo, u, b"x,x,b\r\n"));
        assert!(!scan_one(&combo, u, b"a,x,c\r\n"));
    }

    /// 严格等于·不分大小写：折叠后整字段相等（三路径一致）。
    #[test]
    fn field_scan_strict_eq_ci() {
        let u = Encoding::Utf8;
        // 整字段折叠相等 → 中
        assert!(scan3(&spec_mode(0, "abc", 1), u, b"AbC,x
"));
        // 包含但不等于 → 不中（mode 0 则中）
        assert!(!scan3(&spec_mode(0, "abc", 1), u, b"xabc,x
"));
        assert!(scan3(&spec_mode(0, "abc", 0), u, b"xabc,x
"));
        // 空字段 ≠ 非空关键字；尾字段 / 无尾换行同样生效
        assert!(!scan3(&spec_mode(0, "abc", 1), u, b",x
"));
        assert!(scan3(&spec_mode(1, "abc", 1), u, b",AbC"));
        // 超界列不中
        assert!(!scan3(&spec_mode(9, "abc", 1), u, b"abc,x
"));
    }

    /// 严格等于·区分大小写：比未折叠原始字节（raw_buf 不得被折叠污染）。
    #[test]
    fn field_scan_strict_eq_cs() {
        let u = Encoding::Utf8;
        assert!(!scan3(&spec_mode(0, "abc", 2), u, b"AbC,x
"), "CS：大小写不同即不中");
        assert!(scan3(&spec_mode(0, "AbC", 2), u, b"AbC,x
"));
        assert!(scan3(&spec_mode(0, "abc", 2), u, b"abc,x
"));
        // CJK 无大小写：原样等于
        assert!(scan3(&spec_mode(0, "北京", 2), u, "北京,x
".as_bytes()));
        assert!(!scan3(&spec_mode(0, "北京", 2), u, "北京市,x
".as_bytes()));
    }

    /// 严格等于 × 引号：比较的是剥离引号、坍缩转义后的字段内容。
    #[test]
    fn field_scan_strict_quotes() {
        let u = Encoding::Utf8;
        // 整字段带引：内容 ab 等于 needle ab
        assert!(scan3(&spec_mode(0, "ab", 1), u, b"\"ab\",x
"));
        assert!(scan3(&spec_mode(0, "ab", 2), u, b"\"ab\",x
"));
        // 转义对坍缩："a""b" 的内容是 a"b
        assert!(scan3(&spec_mode(0, "a\"b", 1), u, b"\"a\"\"b\",x
"));
        // 引号内逗号属于内容
        assert!(scan3(&spec_mode(0, "a,b", 1), u, b"\"a,b\",x
"));
        // 未闭合引号吞到记录尾（无尾换行）
        assert!(scan3(&spec_mode(0, "ab", 1), u, b"\"ab"));
        // 含引号字符的内容不等于裸 needle
        assert!(!scan3(&spec_mode(0, "ab", 1), u, b"\"a\"\"b\",x
"));
    }

    /// GBK：严格等于下双字节语义不回归；ASCII needle 永不等于含 GBK 字节的字段。
    ///（GBK 非 ASCII 记录只能走分发/标量，用 scan_one；纯 ASCII 记录可三路径。）
    #[test]
    fn field_scan_strict_gbk() {
        let g = Encoding::Gbk;
        // GBK 非 ASCII 记录路由标量（严格 mode 同样成立）
        let gbk_rec = encoding_rs::GBK.encode("北京,ab
").0.into_owned();
        let ov: HashMap<(u32, ColumnId), String> = HashMap::new();
        let fs = FieldScan::new(&spec_mode(0, "北京", 1), g, &ov, 32);
        assert!(!fs.route_fast(&gbk_rec), "GBK 非 ASCII → 标量（严格模式同）");
        // 标量路径：CJK 严格相等（无大小写折叠问题）
        assert!(scan_one(&spec_mode(0, "北京", 1), g, &gbk_rec));
        assert!(!scan_one(
            &spec_mode(0, "北京", 1),
            g,
            &encoding_rs::GBK.encode("北京市,ab
").0
        ));
        // ASCII needle vs 含 GBK 字节字段：永不相等（引导字节 ≥0x81 天然杜绝）
        assert!(!scan_one(
            &spec_mode(0, "ab", 1),
            g,
            &encoding_rs::GBK.encode("中ab,x
").0
        ));
        assert!(scan_one(
            &spec_mode(1, "ab", 1),
            g,
            &encoding_rs::GBK.encode("中,AB
").0
        ));
        // GBK 纯 ASCII 记录走快路径：严格 CS 语义不串
        assert!(scan3(&spec_mode(0, "Ab", 2), g, b"Ab,x
"));
        assert!(!scan3(&spec_mode(0, "Ab", 2), g, b"ab,x
"));
    }

    // ---- Part 4c：快/慢路径对拍（SIMD 快路径的正确性闸门） ----

    /// 固定种子 LCG（确定性复现；不引新依赖）。
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }

        /// 生成一个随机筛选条件：0-2 根列 needle + 约三成概率多重。
        fn spec(&mut self) -> FilterSpec {
            const KW: &[u8] = b"abzABZ09\", ";
            let mut spec = FilterSpec {
                cols: vec![],
                multi_cols: vec![],
                multi_text: String::new(),
            };
            for _ in 0..self.next() % 3 {
                let kl = 1 + self.next() % 2;
                let kw: String = (0..kl)
                    .map(|_| KW[self.next() as usize % KW.len()] as char)
                    .collect();
                let mode = match self.next() % 4 {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                };
                spec.cols.push((self.next() % 4, kw, mode));
            }
            if self.next() % 10 < 3 {
                spec.multi_cols = vec![self.next() % 3, self.next() % 4];
                spec.multi_text = "a".to_string();
            }
            spec
        }
    }

    /// 同一记录分别走强分发 / 强制快路径 / 强制标量，三者必须逐位一致。
    fn scan_three(spec: &FilterSpec, enc: Encoding, rec: &[u8]) -> (bool, bool, bool) {
        let overlay: HashMap<(u32, ColumnId), String> = HashMap::new();
        let mut a = FieldScan::new(spec, enc, &overlay, 32);
        let mut b = FieldScan::new(spec, enc, &overlay, 32);
        let mut c = FieldScan::new(spec, enc, &overlay, 32);
        (
            a.record_matches(0, rec),
            b.record_matches_fast(0, rec),
            c.record_matches_scalar(0, rec),
        )
    }

    /// 三路径（强分发 / 强制快 / 强制标量）一致性断言并返回结果。
    /// 注意：GBK 非 ASCII 记录不得走 scan3（快路径不处理 GBK 吞字节，
    /// 生产上由分发兜底），GBK 非 ASCII 用例请用 scan_one。
    fn scan3(spec: &FilterSpec, enc: Encoding, rec: &[u8]) -> bool {
        let (a, b, c) = scan_three(spec, enc, rec);
        assert_eq!(a, b, "强分发 vs 强制快路径不一致");
        assert_eq!(b, c, "强制快路径 vs 强制标量不一致");
        a
    }

    /// 路由断言：GBK 非 ASCII 记录走标量；GBK 纯 ASCII 与 UTF-8 走快路径。
    #[test]
    fn field_scan_gbk_non_ascii_routes_to_scalar() {
        let overlay: HashMap<(u32, ColumnId), String> = HashMap::new();
        let gbk = encoding_rs::GBK.encode("名字,值\r\n").0.into_owned();
        let fs = FieldScan::new(&spec_one(0, "名字"), Encoding::Gbk, &overlay, 32);
        assert!(!fs.route_fast(&gbk), "GBK 非 ASCII → 标量慢路径");
        assert!(fs.route_fast(b"a,b\r\n"), "GBK 纯 ASCII → 快路径");
        let fs = FieldScan::new(&spec_one(0, "a"), Encoding::Utf8, &overlay, 32);
        assert!(fs.route_fast("北京,a\r\n".as_bytes()), "UTF-8 → 恒快路径");
    }

    /// 随机对拍：CSV-ish 字节汤 × 随机条件，快路径与标量逐位一致。
    #[test]
    fn field_scan_fast_matches_scalar_random() {
        // 引号与界符权重高，夹杂大小写数字与 UTF-8 片段（可能拼出非法 UTF-8，
        // 两路径同样逐字节处理，不受影响）
        const ALPHABET: &[u8] = b"\"\"\" ,,\r\n\r\nabzABZ09;\xE4\xB8\xAD\xE5\x8C\x97";
        let mut rng = Lcg(0x243F6A8885A308D3);
        for case in 0..6000u32 {
            let len = (rng.next() % 48) as usize;
            let rec: Vec<u8> = (0..len)
                .map(|_| ALPHABET[rng.next() as usize % ALPHABET.len()])
                .collect();
            let spec = rng.spec();
            let (disp, fast, scalar) = scan_three(&spec, Encoding::Utf8, &rec);
            assert_eq!(fast, scalar, "case {case} rec={rec:?}");
            assert_eq!(disp, scalar, "case {case} 分发 rec={rec:?}");
        }
        // GBK 纯 ASCII 字节汤（去掉末尾 6 个 ≥0x80 字节）也走快路径
        let ascii_end = ALPHABET.len() - 6;
        for case in 0..2000u32 {
            let len = (rng.next() % 48) as usize;
            let rec: Vec<u8> = (0..len)
                .map(|_| ALPHABET[rng.next() as usize % ascii_end])
                .collect();
            let spec = rng.spec();
            let (disp, fast, scalar) = scan_three(&spec, Encoding::Gbk, &rec);
            assert_eq!(fast, scalar, "gbk case {case} rec={rec:?}");
            assert_eq!(disp, scalar, "gbk case {case} 分发 rec={rec:?}");
        }
    }

    /// 无列投影缓存：同列换关键字整列重扫；前缀收窄只扫旧命中；编辑/撤销后退回全扫。
    #[test]
    fn big_filter_no_cache_rescan_and_prefix_narrowing() {
        let p = tmp_csv("fastcache", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();

        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 5);

        // 同列新关键字：无缓存可用，整列重扫（结果不变）
        assert_eq!(bf.run_filter(&spec_one(2, "shang")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 5);

        // 前缀收窄：上次 "shang" 命中 2 行，本次只扫这 2 行
        assert_eq!(bf.run_filter(&spec_one(2, "shangha")).unwrap(), 2);
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 2, "前缀收窄应只扫旧命中行");

        // 完全相同条件：直接返回，0 行扫描
        assert_eq!(bf.run_filter(&spec_one(2, "shangha")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 0);

        // 编辑该列（Alice Beijing → Shanghai）→ 收窄基准失效，必须全扫
        bf.set_cell(0, 2, "Shanghai".into(), Approval::deny()).unwrap();
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 1);
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 5, "编辑后应退回全扫，不能用旧命中收窄");

        // 撤销：值回到 Beijing，重新全扫后命中恢复 2
        assert!(bf.undo());
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5);

        fs::remove_file(&p).ok();
    }

    /// 多列筛选不建任何列缓存；插入行不进索引但仍参与匹配。
    #[test]
    fn big_filter_no_cache_multi_column_and_inserted_rows() {
        let p = tmp_csv("fastmulti", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let spec = FilterSpec {
            cols: vec![(0, "a".into(), 0), (2, "beijing".into(), 0)],
            multi_cols: vec![],
            multi_text: String::new(),
        };
        assert_eq!(bf.run_filter(&spec).unwrap(), 2);

        // 纯多重筛选：直扫命中
        assert_eq!(bf.run_filter(&spec_multi(vec![0, 2], "carol")).unwrap(), 1);

        // 插入一行（无源记录），仍须被 OR 命中
        bf.insert_rows(
            bf.meta().rows,
            Some(vec![vec!["carol2".into(), "99".into(), "Beijing".into()]]),
            1,
            Approval::deny(),
        )
        .unwrap();
        assert_eq!(bf.run_filter(&spec_multi(vec![0, 2], "carol")).unwrap(), 2);
        assert_eq!(bf.last_filter_stats().scanned_rows, 6);

        fs::remove_file(&p).ok();
    }

    /// 严格等于：覆盖层（编辑值）三态——CI 折叠等、CS 原样等。
    #[test]
    fn big_filter_strict_overlay() {
        let p = tmp_csv("strict_ov", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 0, "AbC".into(), Approval::deny()).unwrap(); // Alice → AbC（覆盖层）
        // 包含（现状）：折叠子串
        assert_eq!(bf.run_filter(&spec_mode(0, "abc", 0)).unwrap(), 1);
        // 严格·不分大小写：折叠后整字段相等
        assert_eq!(bf.run_filter(&spec_mode(0, "abc", 1)).unwrap(), 1);
        // 严格·区分大小写：abc ≠ AbC
        assert_eq!(bf.run_filter(&spec_mode(0, "abc", 2)).unwrap(), 0);
        assert_eq!(bf.run_filter(&spec_mode(0, "AbC", 2)).unwrap(), 1);
        fs::remove_file(&p).ok();
    }

    /// 插入行（无源记录，走 spec_matches）同样遵守三态。
    #[test]
    fn big_filter_strict_inserted_row() {
        let p = tmp_csv("strict_ins", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        bf.insert_rows(
            bf.meta().rows,
            Some(vec![vec!["Exact".into(), "1".into(), "Town".into()]]),
            1,
            Approval::deny(),
        )
        .unwrap();
        assert_eq!(bf.run_filter(&spec_mode(0, "exact", 1)).unwrap(), 1, "CI 折叠等应命中插入行");
        assert_eq!(bf.run_filter(&spec_mode(0, "exact", 2)).unwrap(), 0, "CS 原样等不应命中");
        assert_eq!(bf.run_filter(&spec_mode(0, "Exact", 2)).unwrap(), 1, "CS 原样等应命中");
        fs::remove_file(&p).ok();
    }

    /// 严格等于破坏前缀单调（等于("ab") 与等于("abc") 不相交）：
    /// 任一侧含严格列 → 禁止收窄，必须全量重扫。
    #[test]
    fn big_filter_strict_disables_narrowing() {
        let p = tmp_csv("strict_narrow", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // 收窄基准：包含 "ali" → Alice
        assert_eq!(bf.run_filter(&spec_one(0, "ali")).unwrap(), 1);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5);
        // 同关键字切到严格：ali ≠ alice → 0 命中，且必须全扫（不可在 1 行上收窄）
        assert_eq!(bf.run_filter(&spec_mode(0, "ali", 1)).unwrap(), 0);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5, "含严格列禁止收窄");
        // 严格 → 同关键字回包含：同样不可收窄
        assert_eq!(bf.run_filter(&spec_one(0, "ali")).unwrap(), 1);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5, "严格切回包含也必须全扫");
        // 严格下逐字追加：等于("alic") 与等于("alice") 不相交，禁止收窄
        assert_eq!(bf.run_filter(&spec_mode(0, "alic", 1)).unwrap(), 0);
        assert_eq!(bf.run_filter(&spec_mode(0, "alice", 1)).unwrap(), 1);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5, "严格下追加字符也必须全扫");
        fs::remove_file(&p).ok();
    }

    /// 同条件短路：spec 相同（含 mode）→ 0 行扫描；mode 翻转是不同条件，必须重扫。
    #[test]
    fn big_filter_strict_short_circuit_mode_change() {
        let p = tmp_csv("strict_sc", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.run_filter(&spec_mode(0, "alice", 1)).unwrap(), 1);
        assert_eq!(bf.last_filter_stats().scanned_rows, 5);
        // 完全相同（含 mode）→ 短路
        assert_eq!(bf.run_filter(&spec_mode(0, "alice", 1)).unwrap(), 1);
        assert_eq!(bf.last_filter_stats().scanned_rows, 0, "同 spec 应短路");
        // mode 1→2：条件变了，必须重扫（即便关键字相同）
        assert_eq!(bf.run_filter(&spec_mode(0, "alice", 2)).unwrap(), 0, "Alice 原样≠alice");
        assert_eq!(bf.last_filter_stats().scanned_rows, 5, "mode 变化不得短路");
        fs::remove_file(&p).ok();
    }

    /// 列严格 + 多重筛选：multi 恒为包含匹配，不受列严格影响。
    #[test]
    fn big_filter_strict_multi_stays_contains() {
        let p = tmp_csv("strict_multi", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // city 严格等于 beijing（Alice+Carol）且 name 包含 carol（multi OR）→ Carol
        let spec = FilterSpec {
            cols: vec![(2, "beijing".into(), 1)],
            multi_cols: vec![0],
            multi_text: "carol".into(),
        };
        assert_eq!(bf.run_filter(&spec).unwrap(), 1, "multi 仍是包含：仅 Carol 命中");
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
        assert_eq!(bf.filter_page(0, 10).rows.len(), 2);

        let r = bf
            .run_filter_cancelable(&spec_one(2, "shanghai"), &gate, 7)
            .unwrap();
        assert_eq!(r, Some(2), "代数一致应正常完成");

        fs::remove_file(&p).ok();
    }

    // ---- Part 5：无列名模式（首行非列名） ----

    fn headerless_csv() -> Vec<u8> {
        b"name,age,city\r\nAlice,30,Beijing\r\nbob,25,Shanghai\r\n".to_vec()
    }

    #[test]
    fn big_open_headerless_keeps_first_record_as_data() {
        let p = tmp_csv("hlopen", &headerless_csv());
        let bf = BigFile::open_with(&p, true).unwrap();
        let m = bf.meta();
        assert!(m.headerless);
        assert_eq!(m.rows, 3);
        assert_eq!(m.cols, 3);
        assert_eq!(m.headers, vec!["列1", "列2", "列3"]);
        assert_eq!(bf.rows(0, 10)[0], vec!["name", "age", "city"]);
        // 对照：默认模式首行是表头
        let bf2 = BigFile::open(&p).unwrap();
        let m2 = bf2.meta();
        assert!(!m2.headerless);
        assert_eq!(m2.rows, 2);
        assert_eq!(m2.headers, vec!["name", "age", "city"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_open_headerless_single_record_roundtrip() {
        let p = tmp_csv("hlone", b"only,row\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf.meta().rows, 1);
        assert_eq!(bf.rows(0, 1)[0], vec!["only", "row"]);
        bf.set_headerless(false).unwrap();
        assert_eq!(bf.meta().rows, 0);
        assert_eq!(bf.meta().headers, vec!["only", "row"]);
        // 变成表头后无行：再开启会把表头放回为数据行
        bf.set_headerless(true).unwrap();
        assert_eq!(bf.meta().rows, 1);
        assert_eq!(bf.rows(0, 1)[0], vec!["only", "row"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_bom_skipped_once_and_saved_once() {
        let p = tmp_csv("hlbom", b"\xEF\xBB\xBFa,b\r\n1,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"], "首格不得含 U+FEFF");
        bf.set_cell(0, 0, "A".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let out = fs::read(&p).unwrap();
        assert!(out.starts_with(&[0xEF, 0xBB, 0xBF]), "应保留原 BOM");
        assert!(!out[3..].starts_with(&[0xEF, 0xBB, 0xBF]), "BOM 只能有一个");
        assert_eq!(&out[3..], b"A,b\r\n1,2\r\n");
        let bf2 = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf2.rows(0, 2), vec![vec!["A", "b"], vec!["1", "2"]]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_bom_leading_blank_not_over_skipped() {
        // 防御性回归：若索引把 BOM 所在的前导行当空行跳过，记录 0 起点就
        // 不在字节 0，此时不能再盲跳 3 字节。真实 build_index 因 BOM 字节
        // 非空不会跳过该行，这里手工构造该索引形态验证 src_span。
        let p = tmp_csv("hlbomblank", b"\xEF\xBB\xBF\r\na,b\r\n1,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        bf.idx.rec_lines.remove(0); // 模拟"前导 BOM 行被跳过"
        bf.seg = Segments::new(0, 2);
        bf.base_ncols = 2;
        bf.base_headers = generic_headers(2);
        bf.schema = Arc::new(Schema::from_headers(generic_headers(2)).unwrap());
        assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
        bf.set_cell(0, 1, "B".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let out = fs::read(&p).unwrap();
        assert!(out.starts_with(&[0xEF, 0xBB, 0xBF]), "保存应保留 BOM");
        assert_eq!(&out[3..], b"a,B\r\n1,2\r\n");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_toggle_roundtrip_promotes_edited_row() {
        let p = tmp_csv("hltoggle", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.meta().rows, 5);
        bf.set_headerless(true).unwrap();
        let m = bf.meta();
        assert!(m.headerless);
        assert_eq!(m.rows, 6, "原表头进入数据");
        assert_eq!(m.headers, vec!["列1", "列2", "列3"]);
        assert_eq!(bf.rows(0, 1)[0], vec!["name", "age", "city"]);
        bf.set_cell(0, 0, "NAME".into(), Approval::deny()).unwrap();
        bf.set_headerless(false).unwrap();
        let m = bf.meta();
        assert!(!m.headerless);
        assert_eq!(m.rows, 5);
        assert_eq!(m.headers, vec!["NAME", "age", "city"], "应含未保存编辑");
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "30", "Beijing"]);
        // 再次开启：首行回到数据
        bf.set_headerless(true).unwrap();
        assert_eq!(bf.meta().rows, 6);
        assert_eq!(bf.rows(0, 1)[0], vec!["NAME", "age", "city"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_insert_at_top_then_disable_keeps_record0_as_data() {
        let p = tmp_csv("hltop", b"a,b\r\n1,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        bf.insert_rows(0, Some(vec![vec!["X".into(), "9".into()]]), 0, Approval::deny())
            .unwrap();
        bf.set_headerless(false).unwrap();
        assert_eq!(bf.meta().headers, vec!["X", "9"]);
        assert_eq!(bf.meta().rows, 2);
        // 被提升的是插入行；物理记录 0 仍是数据（读路径不得假设数据从 1 开始）
        assert_eq!(bf.rows(0, 2), vec![vec!["a", "b"], vec!["1", "2"]]);
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.meta().headers, vec!["X", "9"]);
        assert_eq!(bf2.rows(0, 2), vec![vec!["a", "b"], vec!["1", "2"]]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_filter_hits_first_row_without_cache() {
        let p = tmp_csv("hlfilter", b"alpha,1\r\nbeta,2\r\ngamma,3\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf.run_filter(&spec_one(0, "alpha")).unwrap(), 1);
        assert_eq!(
            bf.filter_page(0, 10).rows[0],
            vec!["alpha", "1"],
            "首行参与筛选"
        );
        let st = bf.last_filter_stats();
        assert_eq!(st.scanned_rows, 3);
        // 同列新关键字直扫，命中正确
        assert_eq!(bf.run_filter(&spec_one(0, "bet")).unwrap(), 1);
        assert_eq!(bf.filter_page(0, 1).rows[0], vec!["beta", "2"]);
        // 编辑首行后仍能命中
        bf.set_cell(0, 0, "ALPHA".into(), Approval::deny()).unwrap();
        assert_eq!(bf.run_filter(&spec_one(0, "alp")).unwrap(), 1);
        assert_eq!(bf.filter_page(0, 1).rows[0][0], "ALPHA");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_save_writes_no_header_line() {
        let p = tmp_csv("hlsave", b"x,1\r\ny,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        bf.set_cell(1, 0, "Y".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x,1\r\nY,2\r\n", "不得注入表头");
        let bf2 = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf2.meta().rows, 2);
        assert_eq!(bf2.rows(0, 2), vec![vec!["x", "1"], vec!["Y", "2"]]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_gbk_first_row_and_save() {
        let (enc, _, _) = encoding_rs::GBK.encode("张三,1\r\n李四,2\r\n");
        let p = tmp_csv("hlgbk", &enc.into_owned());
        let mut bf = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf.meta().encoding, "gbk");
        assert_eq!(bf.rows(0, 1)[0], vec!["张三", "1"]);
        bf.set_cell(0, 1, "9".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf2.meta().encoding, "gbk");
        assert_eq!(
            bf2.rows(0, 2),
            vec![vec!["张三", "9"], vec!["李四", "2"]]
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_cannot_remove_last_row() {
        let p = tmp_csv("hllast", b"a,1\r\nb,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        let err = bf.remove_rows(vec![0, 1], Approval::deny()).unwrap_err();
        assert!(err.contains("至少保留一行"), "{err}");
        assert_eq!(bf.meta().rows, 2, "拒绝后行不变");
        assert_eq!(bf.remove_rows(vec![0], Approval::deny()).unwrap().removed_rows, 1);
        assert_eq!(bf.meta().rows, 1);
        let err = bf.remove_rows(vec![0], Approval::deny()).unwrap_err();
        assert!(err.contains("至少保留一行"), "{err}");
        assert_eq!(bf.meta().rows, 1);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_col_ops_and_rename_rejection() {
        let p = tmp_csv("hlcols", b"a,1\r\nb,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        let err = bf
            .push_col_op(ColOp::Rename { col: 0, name: "名".into() })
            .unwrap_err();
        assert!(err.contains("不可重命名"), "{err}");
        // 待执行列操作期间禁止切换模式
        bf.push_col_op(ColOp::Insert { pos: 2, name: "新列".into() })
            .unwrap();
        let err = bf.set_headerless(false).unwrap_err();
        assert!(err.contains("列操作"), "{err}");
        assert!(bf.meta().headerless);
        bf.apply_col_ops(|_, _, _, _| {}).unwrap();
        let m = bf.meta();
        assert!(m.headerless);
        assert_eq!(m.cols, 3);
        assert_eq!(m.rows, 2);
        assert_eq!(bf.rows(0, 1)[0], vec!["a", "1", ""]);
        // 文件不得写独立表头：重开无列名，两行都是数据
        let bf2 = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf2.meta().rows, 2);
        assert_eq!(
            bf2.rows(0, 2),
            vec![vec!["a", "1", ""], vec!["b", "2", ""]]
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_rejects_zero_columns_on_col_ops() {
        let p = tmp_csv("hlzerocol", b"a\r\nb\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        bf.push_col_op(ColOp::Delete { col: 0 }).unwrap();
        let err = bf.apply_col_ops(|_, _, _, _| {}).unwrap_err();
        assert!(err.contains("至少保留一列"), "{err}");
        assert!(bf.meta().pending_cols, "失败时保留待执行操作");
        assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\r\n", "文件未被改写");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_rejects_zero_rows_on_disable_save_and_col_ops() {
        let p = tmp_csv("hlzerorow", b"a,1\r\nb,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        // 正常路径已禁止删最后一行；这里直接构造 0 行状态验证兜底
        bf.seg = Segments::new(0, 0);
        let err = bf.set_headerless(false).unwrap_err();
        assert!(err.contains("没有数据行可作为列名"), "{err}");
        assert!(bf.meta().headerless, "失败不得切换模式");
        let err = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap_err();
        assert!(err.contains("至少保留一行"), "{err}");
        bf.push_col_op(ColOp::Insert { pos: 0, name: "x".into() })
            .unwrap();
        let err = bf.apply_col_ops(|_, _, _, _| {}).unwrap_err();
        assert!(err.contains("至少保留一行"), "{err}");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_toggle_clears_filter_and_undo() {
        let p = tmp_csv("hlclear", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        assert_eq!(bf.run_filter(&spec_one(2, "beijing")).unwrap(), 2);
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        assert!(bf.meta().can_undo);
        bf.set_headerless(true).unwrap();
        assert!(!bf.has_filter(), "切换后筛选清空");
        assert_eq!(bf.filter_len(), 0);
        assert!(!bf.meta().can_undo, "切换清空撤销");
        assert!(!bf.meta().can_redo);
        // 开关是视图属性：不改变文件
        assert_eq!(fs::read(&p).unwrap(), people_csv());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_headerless_progressive_open_and_toggle() {
        let mut s = String::from("a,b\r\n");
        for i in 0..20 {
            s.push_str(&format!("{i:04},x\r\n"));
        }
        let bytes = s.into_bytes();
        let p = tmp_csv("hlprog", &bytes);
        // 前缀 29B = 表头 + 3 行；无列名时 4 条记录全是数据
        let mut bf = BigFile::open_progressive_full(&p, 29, 1000, 8, false, true).unwrap();
        let m = bf.meta();
        assert!(m.indexing);
        assert!(m.headerless);
        assert_eq!(m.rows, 4);
        assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
        assert_eq!(bf.copy_tsv(0, 1, 0, 2).unwrap(), "a\tb\r\n");
        // 索引期关闭开关：只翻 offset，首行变表头
        bf.set_headerless(false).unwrap();
        assert_eq!(bf.meta().rows, 3);
        assert_eq!(bf.meta().headers, vec!["a", "b"]);
        assert_eq!(bf.rows(0, 1)[0], vec!["0000", "x"]);
        bf.set_headerless(true).unwrap();
        assert_eq!(bf.meta().rows, 4);
        // 完成索引后段表从物理记录 0 开始
        bf.drive_index_for_test();
        assert!(!bf.meta().indexing);
        bf.refresh_indexed();
        assert_eq!(bf.meta().rows, 21);
        assert_eq!(bf.rows(0, 1)[0], vec!["a", "b"]);
        assert_eq!(bf.rows(20, 1)[0], vec!["0019", "x"]);
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

    // ---- 排序模式：内存后备（Backing::Ram） ----

    #[test]
    fn ram_load_switches_backing_and_keeps_rows() {
        let p = tmp_csv("ram_load", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        assert!(!bf.is_ram_backed());
        let size = fs::metadata(&p).unwrap().len();
        let est = bf.sort_estimate(1);
        assert_eq!(est.file_bytes, size);
        assert_eq!(est.row_count, 5);
        assert!(est.new_bytes > est.file_bytes, "估算应含缓冲之外的键/排列开销");
        assert!(est.peak_bytes >= est.file_bytes * 2, "峰值应体现映射页缓存");
        let cancel = AtomicBool::new(false);
        let mut last = (0u64, 0u64);
        let loaded = bf
            .load_into_ram(&cancel, &mut |done, total| last = (done, total))
            .unwrap();
        assert!(loaded);
        assert!(bf.is_ram_backed());
        assert!(bf.meta().ram_backed);
        assert_eq!(last, (size, size), "最后一次进度应为满");
        assert_eq!(bf.rows(0, 5)[0], vec!["Alice", "30", "Beijing"]);
        assert_eq!(bf.rows(4, 1)[0][0], "Eve");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn ram_load_cancel_keeps_mmap_and_data() {
        let p = tmp_csv("ram_cancel", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let cancel = AtomicBool::new(true); // 用户点取消
        let loaded = bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap();
        assert!(!loaded, "取消应返回 false 而不是错误");
        assert!(!bf.is_ram_backed(), "取消后仍用原 mmap 视图");
        assert!(!bf.meta().ram_backed);
        assert_eq!(bf.rows(0, 1)[0][0], "Alice");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn ram_load_progress_is_monotonic_and_reaches_total() {
        let p = tmp_csv("ram_progress", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let cancel = AtomicBool::new(false);
        let mut samples: Vec<(u64, u64)> = Vec::new();
        bf.load_into_ram(&cancel, &mut |d, t| samples.push((d, t)))
            .unwrap();
        assert!(samples.len() >= 2, "至少应报告起点与终点");
        assert_eq!(samples.first().unwrap().0, 0);
        assert!(samples.windows(2).all(|w| w[0].0 <= w[1].0), "进度不得倒退");
        let total = fs::metadata(&p).unwrap().len();
        assert_eq!(samples.last().copied(), Some((total, total)));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn ram_load_refuses_while_indexing() {
        let p = tmp_csv("ram_indexing", &people_csv());
        // 小阈值迫使打开后仍处于渐进索引期（不启后台线程）
        let mut bf = BigFile::open_progressive_with(&p, 8, 1, 16, false).unwrap();
        assert!(bf.is_indexing());
        let cancel = AtomicBool::new(false);
        let err = bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap_err();
        assert!(err.contains("索引"), "错误应说明索引未完成：{err}");
        assert!(!bf.is_ram_backed());
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_estimate_scales_with_levels() {
        let p = tmp_csv("ram_est", &people_csv());
        let bf = BigFile::open(&p).unwrap();
        let one = bf.sort_estimate(1);
        let three = bf.sort_estimate(3);
        assert_eq!(one.file_bytes, three.file_bytes);
        assert!(three.new_bytes > one.new_bytes, "级别越多键内存越大");
        assert_eq!(one.row_count, 5);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn save_after_ram_load_roundtrip() {
        let p = tmp_csv("ram_save", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap());
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        // 保存后回到 mmap 后备（排序模式结束/重建）
        assert!(!bf.is_ram_backed());
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0], vec!["Alice", "31", "Beijing"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn available_memory_is_positive() {
        let n = available_memory_bytes().expect("当前平台应能查询可用内存");
        assert!(n > 0);
    }

    // ---- 排序键与排列层（阶段 4） ----

    /// 排序夹具：id,age,key；含空/非数字/大小写文本。
    fn sort_fixture() -> Vec<u8> {
        b"id,age,key\r\na,10,foo\r\nb,2,bar\r\nc,,foo\r\nd,2,Bar\r\ne,x,foo\r\n"
            .to_vec()
    }

    /// 打开并载入内存缓冲（排序模式前置）。
    fn load_ram(p: &Path) -> BigFile {
        let mut bf = BigFile::open(p).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap());
        bf
    }

    /// 取前 n 行的第一列（排序断言用）。
    fn first_col(bf: &BigFile, n: u32) -> Vec<String> {
        bf.rows(0, n).into_iter().map(|r| r[0].clone()).collect()
    }

    #[test]
    fn sort_numeric_asc_puts_non_numbers_last() {
        let p = tmp_csv("sort_num_asc", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 1, numeric: true, asc: true }])
            .unwrap();
        assert!(bf.meta().sorted);
        // 2(b),2(d),10(a)，非数字（c 空、e x）恒排最后且保持原相对顺序
        assert_eq!(first_col(&bf, 5), vec!["b", "d", "a", "c", "e"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_numeric_desc_still_puts_non_numbers_last() {
        let p = tmp_csv("sort_num_desc", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 1, numeric: true, asc: false }])
            .unwrap();
        assert_eq!(first_col(&bf, 5), vec!["a", "b", "d", "c", "e"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_text_uses_byte_order_and_is_stable() {
        let p = tmp_csv("sort_text", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 2, numeric: false, asc: true }])
            .unwrap();
        // "Bar" < "bar" < "foo"；三个 foo 保持原有 a,c,e 顺序
        assert_eq!(first_col(&bf, 5), vec!["d", "b", "a", "c", "e"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_multilevel_applies_next_level_on_ties() {
        let p = tmp_csv("sort_multi", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[
            SortLevel { col: 2, numeric: false, asc: true },
            SortLevel { col: 1, numeric: true, asc: false },
        ])
        .unwrap();
        // 先按 key：Bar(d), bar(b), foo(a,c,e)；foo 内按 age 降序：10(a) 在前
        assert_eq!(first_col(&bf, 5), vec!["d", "b", "a", "c", "e"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_numeric_matches_js_number_parsing() {
        let p = tmp_csv(
            "sort_jsnum",
            b"k,v\r\na,2\r\nb,0x10\r\nc, 3 \r\nd,\r\ne,abc\r\nf,1e2\r\n",
        );
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 1, numeric: true, asc: true }])
            .unwrap();
        // 2(a) < 3(c) < 0x10=16(b) < 1e2=100(f)；空/abc 非数字最后（d,e）
        assert_eq!(first_col(&bf, 6), vec!["a", "c", "b", "f", "d", "e"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_rebuilds_filter_positions_in_sorted_order() {
        let p = tmp_csv("sort_filter", &sort_fixture());
        let mut bf = load_ram(&p);
        let spec = FilterSpec {
            cols: vec![(2, "foo".into(), 0)],
            multi_cols: Vec::new(),
            multi_text: String::new(),
        };
        assert_eq!(bf.run_filter(&spec).unwrap(), 3); // a,c,e
        // 按 id 降序：e,d,c,b,a
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap();
        assert_eq!(bf.filter_len(), 3, "排序后命中集不应丢失");
        let page = bf.filter_page(0, 3).rows;
        assert_eq!(
            page.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
            vec!["e", "c", "a"],
            "命中行应按排序后的显示顺序返回"
        );
        // 新条件在排序视图上重扫：age 含 2 → d(1), b(3)
        let spec2 = FilterSpec {
            cols: vec![(1, "2".into(), 0)],
            multi_cols: Vec::new(),
            multi_text: String::new(),
        };
        assert_eq!(bf.run_filter(&spec2).unwrap(), 2);
        let page2 = bf.filter_page(0, 2).rows;
        assert_eq!(
            page2.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
            vec!["d", "b"]
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_edit_then_refilter_after_clear_still_matches() {
        let p = tmp_csv("sort_refilt", &sort_fixture());
        let mut bf = load_ram(&p);
        // 按 id 降序：e,d,c,b,a
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap();
        let spec_b = FilterSpec {
            cols: vec![(0, "b".into(), 0)],
            multi_cols: Vec::new(),
            multi_text: String::new(),
        };
        assert_eq!(bf.run_filter(&spec_b).unwrap(), 1);
        bf.clear_filter();
        // 编辑显示首行（e）的 age：不得影响按 id 重筛
        bf.set_cell(0, 1, "99".into(), Approval::deny()).unwrap();
        assert_eq!(
            bf.run_filter(&spec_b).unwrap(),
            1,
            "清筛+编辑后重筛仍应命中 b"
        );
        assert_eq!(bf.filter_page(0, 1).rows[0][0], "b");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_then_edit_undo_and_copy_use_display_rows() {
        let p = tmp_csv("sort_edit", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap(); // e,d,c,b,a
        assert_eq!(bf.copy_tsv(0, 2, 0, 2).unwrap(), "e\tx\r\nd\t2\r\n");
        bf.set_cell(0, 0, "E2".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(0, 1)[0], vec!["E2", "x", "foo"]);
        bf.undo();
        assert_eq!(bf.rows(0, 1)[0][0], "e", "撤销应按稳定身份回到原行");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_keeps_edits_and_inserted_rows() {
        let p = tmp_csv("sort_ins", &sort_fixture());
        let mut bf = BigFile::open(&p).unwrap();
        bf.set_cell(0, 1, "99".into(), Approval::deny()).unwrap(); // a 的 age
        bf.insert_rows(
            0,
            Some(vec![vec!["z".into(), "1".into(), "foo".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap();
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap();
        assert_eq!(first_col(&bf, 6), vec!["a", "b", "c", "d", "e", "z"]);
        assert_eq!(bf.rows(0, 1)[0][1], "99", "覆盖层编辑应跟随记录");
        assert_eq!(bf.rows(5, 1)[0], vec!["z", "1", "foo"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_requires_ram_and_valid_levels() {
        let p = tmp_csv("sort_bad", &sort_fixture());
        let mut bf = BigFile::open(&p).unwrap();
        let err = bf
            .sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap_err();
        assert!(err.contains("载入内存"), "未载入应拒绝：{err}");
        let cancel = AtomicBool::new(false);
        bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap();
        assert!(bf.sort_by_levels(&[]).unwrap_err().contains("排序级别"));
        assert!(
            bf.sort_by_levels(&[SortLevel { col: 9, numeric: false, asc: true }])
                .unwrap_err()
                .contains("列")
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_blocks_structural_ops_until_exit() {
        let p = tmp_csv("sort_block", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap();
        assert!(bf.insert_rows(0, None, 1, Approval::deny()).unwrap_err().contains("排序"));
        assert!(bf.remove_rows(vec![0], Approval::deny()).unwrap_err().contains("排序"));
        assert!(
            bf.push_col_op(ColOp::Delete { col: 0 })
                .unwrap_err()
                .contains("排序")
        );
        assert!(bf.set_headerless(true).unwrap_err().contains("排序"));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn sort_exit_restores_view_and_releases_ram() {
        let p = tmp_csv("sort_exit", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.set_cell(0, 1, "99".into(), Approval::deny()).unwrap();
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap();
        bf.exit_sort_mode(true).unwrap();
        assert!(!bf.meta().sorted);
        assert!(!bf.is_ram_backed(), "退出并要求释放后应回到 mmap");
        assert_eq!(first_col(&bf, 5), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(bf.rows(0, 1)[0][1], "99", "退出排序保留编辑");
        // 退出后结构操作恢复可用
        bf.insert_rows(0, None, 1, Approval::deny()).unwrap();
        assert_eq!(bf.meta().rows, 6);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn save_sorted_writes_physical_order_and_exits_sort() {
        let p = tmp_csv("sort_save", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.set_cell(0, 1, "77".into(), Approval::deny()).unwrap(); // a 行（原显示位置 0）
        bf.insert_rows(0, Some(vec![vec!["z".into(), "5".into(), "foo".into()]]), 0, Approval::deny())
            .unwrap();
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: false }])
            .unwrap(); // z,e,d,c,b,a
        assert!(bf.meta().sorted);
        // 进入排序已经建立固定点并清空此前历史，保存无需再次确认。
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(!bf.meta().sorted, "保存后应退出排序视图");
        assert!(!bf.is_ram_backed());
        assert_eq!(first_col(&bf, 6), vec!["z", "e", "d", "c", "b", "a"]);
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(first_col(&bf2, 6), vec!["z", "e", "d", "c", "b", "a"]);
        assert_eq!(bf2.rows(5, 1)[0][1], "77", "排序保存应带上单元格编辑");
        assert_eq!(bf2.rows(0, 1)[0][1], "5", "排序保存应带上插入行");
        // 物理顺序已重排：文件头之后第一行就是 z
        let text = String::from_utf8(fs::read(&p).unwrap()).unwrap();
        assert!(text.starts_with("id,age,key\r\nz,5,foo\r\n"), "实际开头：{text:?}");
        fs::remove_file(&p).ok();
    }

    #[test]
    fn save_sorted_headerless_keeps_first_row_as_data() {
        let p = tmp_csv("sort_nohdr", b"c,3\r\na,1\r\nb,2\r\n");
        let mut bf = BigFile::open_with(&p, true).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(bf.load_into_ram(&cancel, &mut |_, _| {}).unwrap());
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap();
        bf.save(Approval::deny(), &mut |_, _, _, _| {}).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"a,1\r\nb,2\r\nc,3\r\n");
        // 重开仍按无列名解释：三行全是数据，没有被注入表头
        let bf2 = BigFile::open_with(&p, true).unwrap();
        assert_eq!(bf2.meta().rows, 3);
        assert_eq!(first_col(&bf2, 3), vec!["a", "b", "c"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn save_sorted_handles_missing_final_newline() {
        // 末行无换行：排序后它可能落在中间，必须补行尾否则两行粘连。
        let p = tmp_csv("sort_save_nonl", b"id,v\r\nb,2\r\na,1");
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 2), vec![
            vec!["a".to_string(), "1".to_string()],
            vec!["b".to_string(), "2".to_string()],
        ]);
        fs::remove_file(&p).ok();
    }


    #[test]
    fn save_sorted_history_requires_fixed_point_confirmation() {
        let p = tmp_csv("sort_save_fixed_point", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }]).unwrap();
        bf.set_cell(0, 0, "A".into(), Approval::deny()).unwrap();
        let before = std::fs::read(&p).unwrap();

        let denied = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(!denied.saved);
        let request = denied.fixed_point.expect("排序保存必须请求固定点");
        assert_eq!(request.kind, FixedPointKind::SortMaterialization);
        assert!(bf.meta().can_undo && bf.meta().modified);
        assert_eq!(std::fs::read(&p).unwrap(), before);

        let saved = bf.save(Approval::allow(request.preflight_id), |_, _, _, _| {}).unwrap();
        assert!(saved.saved);
        assert!(!bf.meta().can_undo && !bf.meta().modified);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn save_sorted_clears_undo_history() {
        let p = tmp_csv("sort_save_undo", &sort_fixture());
        let mut bf = load_ram(&p);
        bf.sort_by_levels(&[SortLevel { col: 0, numeric: false, asc: true }])
            .unwrap();
        assert!(!bf.meta().modified, "排序只改视图，不算内容修改");
        bf.set_cell(0, 0, "A".into(), Approval::deny()).unwrap();
        assert!(bf.meta().can_undo);
        assert!(bf.meta().modified);
        let denied = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(!denied.saved);
        assert!(bf.meta().can_undo, "取消确认前历史必须保留");
        let request = denied.fixed_point.unwrap();
        bf.save(Approval::allow(request.preflight_id), |_, _, _, _| {}).unwrap();
        assert!(!bf.meta().can_undo, "物理重排后旧记录号失效，撤销栈必须清空");
        assert!(!bf.meta().modified, "保存后回到干净状态");
        fs::remove_file(&p).ok();
    }

    /// 打印一次筛选的耗时与引擎统计（含进程私有内存——验证直扫 mmap 零增长）。
    fn bench_timed(bf: &mut BigFile, spec: &FilterSpec, label: &str) {
        let t = std::time::Instant::now();
        let n = bf.run_filter(spec).unwrap();
        let st = bf.last_filter_stats();
        eprintln!(
            "{label:<28} 命中 {n:>9} | 扫 {:>9} 行 | 私有 {:>7.1}MB | {:?}",
            st.scanned_rows,
            private_bytes() as f64 / (1u64 << 20) as f64,
            t.elapsed()
        );
    }

    /// 当前进程私有提交内存（字节）。Windows 走 K32GetProcessMemoryInfo
    ///（kernel32，无新依赖）；其他平台恒 0（基准只在本机手工跑）。
    #[cfg(windows)]
    fn private_bytes() -> u64 {
        #[repr(C)]
        struct Pmc {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_nonpaged_pool_usage: usize,
            quota_nonpaged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        extern "C" {
            fn GetCurrentProcess() -> *mut core::ffi::c_void;
            fn K32GetProcessMemoryInfo(
                process: *mut core::ffi::c_void,
                counters: *mut Pmc,
                cb: u32,
            ) -> i32;
        }
        let mut pmc: Pmc = unsafe { std::mem::zeroed() };
        pmc.cb = std::mem::size_of::<Pmc>() as u32;
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
        if ok != 0 {
            pmc.pagefile_usage as u64
        } else {
            0
        }
    }

    /// 非 Windows 平台占位（基准只在本机手工跑）。
    #[cfg(not(windows))]
    fn private_bytes() -> u64 {
        0
    }

    /// 大文件筛选基准：首筛直扫 / 同列重扫 / 前缀收窄 / 中文关键字。
    /// 每次筛选打印进程私有内存——无列投影缓存，多列筛选后增量应 ≈ 0。
    /// `cargo test --release --lib bench_big_filter -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_big_filter() {
        let (path, _) = bench_env();
        let t = std::time::Instant::now();
        let mut bf = BigFile::open(&path).unwrap();
        eprintln!("open: {:?}, rows={}", t.elapsed(), bf.meta().rows);

        // 1) 首筛（整表字节直扫，不建任何投影）
        bench_timed(&mut bf, &spec_one(1, "user-377"), "首筛 ASCII user-377");
        // 2) 同列换关键字：无缓存，整列重扫
        bench_timed(&mut bf, &spec_one(1, "user-378"), "同列重扫 user-378");
        // 3) 逐字收窄（只扫上次命中行）
        bench_timed(&mut bf, &spec_one(1, "user-37"), "收窄基 user-37");
        bench_timed(&mut bf, &spec_one(1, "user-377"), "收窄 user-377");
        // 4) 长备注列首筛
        bench_timed(&mut bf, &spec_one(6, "batch-777"), "首筛 note batch-777");
        // 5) 前缀收窄（上一轮命中行）
        bench_timed(&mut bf, &spec_one(6, "batch-777 "), "收窄 batch-777 空格");
        // 6) 中文关键字首筛
        bench_timed(&mut bf, &spec_one(2, "朝阳"), "首筛 CJK 朝阳");
        // 7) 中文同列重扫
        bench_timed(&mut bf, &spec_one(2, "浦东"), "同列重扫 CJK 浦东");
        // 8) 严格等于 CI：长度预检 + memcmp，应不比包含慢
        bench_timed(&mut bf, &spec_mode(1, "user-377", 1), "首筛严格CI user-377");
        // 9) 严格等于 CS：raw_buf 填充路径
        bench_timed(&mut bf, &spec_mode(1, "user-377", 2), "首筛严格CS user-377");
    }

    // ---- Part 24：内存字节源建表（统一大引擎，新建表格不写盘） ----

    /// from_bytes：内存字节直接建会话——无路径、Ram 后备、同步索引，
    /// meta/读路径与磁盘打开完全一致。
    #[test]
    fn from_bytes_meta_matches_disk_open() {
        let bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let m = bf.meta();
        assert_eq!(m.rows, 5);
        assert_eq!(m.cols, 3);
        assert_eq!(m.headers, vec!["name", "age", "city"]);
        assert_eq!(m.encoding, "utf-8");
        assert!(!m.headerless && !m.indexing && m.index_sync);
        assert!(m.ram_backed && !m.has_path);
        assert!(bf.is_ram_backed());
        assert!(!bf.has_path());
        assert!(!bf.stale(), "无路径内存档不参与外部改动检测");
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "30", "Beijing"]);
    }

    /// from_bytes 无列名：首条记录是数据，列名用「列N」虚拟表头。
    #[test]
    fn from_bytes_headerless_first_record_is_data() {
        let bf = BigFile::from_bytes(people_csv(), true).unwrap();
        let m = bf.meta();
        assert!(m.headerless);
        assert_eq!(m.rows, 6);
        assert_eq!(m.headers, vec!["列1", "列2", "列3"]);
        assert_eq!(bf.rows(0, 1)[0], vec!["name", "age", "city"]);
    }

    /// from_bytes：空缓冲与磁盘空文件同语义拒绝。
    #[test]
    fn from_bytes_rejects_empty() {
        assert!(BigFile::from_bytes(Vec::new(), false).is_err());
        assert!(BigFile::from_bytes(Vec::new(), true).is_err());
    }

    /// 内存档编辑全链路：改格、插行、撤销、重做——与 mmap 会话行为一致。
    #[test]
    fn memory_doc_edit_insert_undo_redo() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "31", "Beijing"]);
        bf.insert_rows(1, Some(vec![vec!["New".into(), "20".into(), "Xian".into()]]), 0, Approval::deny())
            .unwrap();
        assert_eq!(bf.meta().rows, 6);
        assert_eq!(bf.rows(1, 1)[0], vec!["New", "20", "Xian"]);
        assert!(bf.undo(), "撤销插行");
        assert_eq!(bf.meta().rows, 5);
        assert!(bf.undo(), "撤销改格");
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "30", "Beijing"]);
        assert!(bf.redo(), "重做改格");
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "31", "Beijing"]);
    }

    /// 内存档筛选/排序/退出排序：无路径档退出排序不得回落 mmap，保持 Ram。
    #[test]
    fn memory_doc_filter_sort_exit_keeps_ram() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        let hits = bf.run_filter(&spec_one(2, "beijing")).unwrap();
        assert_eq!(hits, 2);
        bf.clear_filter();
        assert_eq!(bf.meta().rows, 5);
        bf.sort_by_levels(&[SortLevel { col: 1, numeric: true, asc: false }])
            .unwrap();
        assert_eq!(bf.rows(0, 1)[0][1], "35", "年龄降序首行是 Carol");
        bf.exit_sort_mode(true).unwrap();
        assert!(bf.is_ram_backed(), "无路径内存档退出排序必须保持 Ram 后备");
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "30", "Beijing"]);
    }

    /// 内存档切换首行语义：与磁盘档一致，不因无路径报错。
    #[test]
    fn memory_doc_set_headerless_toggle() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_headerless(true).unwrap();
        let m = bf.meta();
        assert!(m.headerless && m.rows == 6);
        assert_eq!(m.headers, vec!["列1", "列2", "列3"]);
        bf.set_headerless(false).unwrap();
        let m = bf.meta();
        assert!(!m.headerless && m.rows == 5);
        assert_eq!(m.headers, vec!["name", "age", "city"]);
    }

    /// 无路径直存：必须报「请先另存为」，不写任何文件。
    #[test]
    fn save_without_path_requires_save_as() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        let err = bf.save(Approval::deny(), |_, _, _, _| {}).unwrap_err();
        assert!(err.contains("另存为"), "实际错误：{err}");
    }

    /// 另存为落盘：获得路径并回 mmap；此后 save 直写同一路径。
    #[test]
    fn save_as_gains_path_then_save_direct() {
        let p = tmp_csv("mem_save_as", b"placeholder\n");
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        bf.save_as(&p, Approval::deny(), |_, _, _, _| {}).unwrap();
        assert!(bf.has_path());
        assert!(!bf.is_ram_backed(), "另存为成功后应回到 mmap 后备");
        bf.set_cell(1, 1, "26".into(), Approval::deny()).unwrap();
        bf.save(Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(
            bf2.rows(0, 2),
            vec![
                vec!["Alice".to_string(), "31".to_string(), "Beijing".to_string()],
                vec!["bob".to_string(), "26".to_string(), "Shanghai".to_string()],
            ]
        );
        fs::remove_file(&p).ok();
    }

    /// 另存为换名失败（目标被目录占位）：无路径 Ram 档会话必须原样可读
    /// 可编辑——钉死 replace_with_retry 不得无条件丢弃内存字节。
    #[test]
    fn save_as_rename_failure_keeps_memory_session() {
        let dir = std::env::temp_dir().join(format!("st_engine_memfail_{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.set_cell(0, 1, "31".into(), Approval::deny()).unwrap();
        let err = bf.save_as(&dir, Approval::deny(), |_, _, _, _| {}).unwrap_err();
        assert!(!err.is_empty());
        assert!(!bf.has_path() && bf.is_ram_backed());
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "31", "Beijing"]);
        bf.set_cell(0, 1, "32".into(), Approval::deny()).unwrap();
        assert_eq!(bf.rows(0, 1)[0][1], "32");
        let tmp = std::env::temp_dir()
            .join(format!("st_engine_memfail_{}.simpletable-tmp", std::process::id()));
        fs::remove_file(&tmp).ok();
        fs::remove_dir(&dir).ok();
    }

    /// 内存档列操作：apply 在内存内重建（不写临时文件），表头/数据正确、
    /// 仍无路径保持 Ram，且重建后可另存为。
    #[test]
    fn memory_doc_apply_col_ops_rebuilds_in_memory() {
        let mut bf = BigFile::from_bytes(people_csv(), false).unwrap();
        bf.push_col_op(ColOp::Insert { pos: 1, name: "gender".into() })
            .unwrap();
        bf.apply_col_ops(|_, _, _, _| {}).unwrap();
        let m = bf.meta();
        assert_eq!(m.cols, 4);
        assert_eq!(m.headers, vec!["name", "gender", "age", "city"]);
        assert!(m.ram_backed && !m.has_path);
        assert_eq!(bf.rows(0, 1)[0], vec!["Alice", "", "30", "Beijing"]);
        let p = tmp_csv("mem_colops", b"x\n");
        bf.save_as(&p, Approval::deny(), |_, _, _, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0], vec!["Alice", "", "30", "Beijing"]);
        fs::remove_file(&p).ok();
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
    /// 建单段段表：`start` 为首条数据记录的物理记录号
    /// （有表头 = 1，无列名 = 0），`count` 为数据记录数。
    fn new(start: u32, count: u32) -> Self {
        Self {
            segs: if count > 0 {
                vec![Seg { kind: SegKind::Src { start, len: count }, len: count }]
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
    fn delete_one(&mut self, vrow: u64) -> Option<Vec<String>> {
        let (i, off) = self.locate(vrow)?;
        let s = self.segs[i];
        match s.kind {
            SegKind::Src { start, len } => {
                if len != s.len {
                    return None;
                }
                let rec = start.checked_add(off)?;
                let next_start = rec.checked_add(1)?;
                let right_len = len.checked_sub(off)?.checked_sub(1)?;
                self.tombstones.insert(rec);
                // [start,rec) + [rec+1, start+len)：被删记录从段中剔除（位图保留身份）
                self.segs[i] = Seg { kind: SegKind::Src { start, len: off }, len: off };
                self.segs.insert(
                    i + 1,
                    Seg {
                        kind: SegKind::Src { start: next_start, len: right_len },
                        len: right_len,
                    },
                );
                Some(Vec::new())
            }
            SegKind::Ins { block } => {
                let rows = self.blocks.get_mut(block as usize)?.as_mut()?;
                if off as usize >= rows.len() {
                    return None;
                }
                let cells = rows.remove(off as usize);
                if rows.is_empty() {
                    self.blocks[block as usize] = None;
                    self.segs.remove(i);
                } else {
                    self.segs[i].len -= 1;
                }
                Some(cells)
            }
        }
    }

    /// 删除中途发现内部不一致时，按相反顺序恢复已完成的删除。
    fn rollback_deleted(&mut self, mut rollbacks: Vec<DeleteRollback>) {
        while let Some(rollback) = rollbacks.pop() {
            match rollback.seg.kind {
                SegKind::Src { start, .. } => {
                    let rec = start.checked_add(rollback.off).expect("删除回滚记录号有效");
                    if rollback.tombstone_was_present {
                        self.tombstones.insert(rec);
                    } else {
                        self.tombstones.remove(&rec);
                    }
                    self.segs.remove(rollback.index + 1);
                    self.segs[rollback.index] = rollback.seg;
                }
                SegKind::Ins { block } => {
                    let slot = self.blocks.get_mut(block as usize).expect("删除块必须存在");
                    let rows = slot.get_or_insert_with(Vec::new);
                    rows.insert(rollback.off as usize, rollback.cells);
                    if rollback.removed_seg {
                        self.segs.insert(rollback.index, rollback.seg);
                    } else {
                        self.segs[rollback.index] = rollback.seg;
                    }
                }
            }
        }
    }

    /// 写入前按恢复顺序模拟插入块状态，保证后续失败不会留下半次撤销。
    fn validate_restore_inserted(&self, items: &[DeletedRow]) -> bool {
        let mut simulated = HashMap::<u32, (bool, usize, Option<u32>)>::new();
        for item in items {
            let (block, row) = match item {
                DeletedRow::Ins { block, row, .. } => (*block, *row),
                DeletedRow::Suspended { block, .. } => (*block, 0),
                DeletedRow::Src { .. } => continue,
            };
            if !simulated.contains_key(&block) {
                let Some(slot) = self.blocks.get(block as usize) else { return false };
                let occupied = slot.is_some();
                let rows_len = slot.as_ref().map_or(0, Vec::len);
                let seg_len = self.segs.iter().find(|seg| {
                    matches!(seg.kind, SegKind::Ins { block: id } if id == block)
                }).map(|seg| seg.len);
                simulated.insert(block, (occupied, rows_len, seg_len));
            }
            let (occupied, rows_len, seg_len) = simulated.get_mut(&block).unwrap();
            match (*occupied, *seg_len) {
                (true, Some(current_seg_len)) => {
                    let Some(next_len) = current_seg_len.checked_add(1) else { return false };
                    if row as usize > *rows_len || current_seg_len as usize != *rows_len {
                        return false;
                    }
                    *rows_len += 1;
                    *seg_len = Some(next_len);
                }
                (false, None) => {
                    if row != 0 {
                        return false;
                    }
                    *occupied = true;
                    *rows_len = 1;
                    *seg_len = Some(1);
                }
                _ => return false,
            }
        }
        true
    }

    /// 按原块号和块内行号恢复删除的插入行，保留旧历史的 CellId。
    ///
    /// # Parameters
    /// - `vpos`: 整块已删除时用于恢复原段位置的逻辑行号。
    /// - `block`: 删除前的稳定插入块号。
    /// - `row`: 删除前的块内行号。
    /// - `cells`: 待恢复的一整行内容。
    ///
    /// # Returns
    /// 身份和位置一致时恢复并返回 true；内部状态不匹配时返回 false。
    fn restore_inserted(
        &mut self,
        vpos: u64,
        block: u32,
        row: u32,
        cells: Vec<String>,
    ) -> bool {
        let Some(slot) = self.blocks.get(block as usize) else { return false };
        let occupied = slot.is_some();
        let seg_index = self.segs.iter().position(|seg| {
            matches!(seg.kind, SegKind::Ins { block: id } if id == block)
        });
        match (occupied, seg_index) {
            (true, Some(i)) => {
                let rows = self.blocks[block as usize].as_mut().unwrap();
                let next_len = self.segs[i].len.checked_add(1);
                if row as usize > rows.len()
                    || self.segs[i].len as usize != rows.len()
                    || next_len.is_none()
                {
                    return false;
                }
                rows.insert(row as usize, cells);
                self.segs[i].len = next_len.unwrap();
                true
            }
            (false, None) => {
                // 整块删除后的首条恢复必为原第 0 行；后续行走上面的分支。
                if row != 0 {
                    return false;
                }
                let at = self.split_at(vpos);
                self.blocks[block as usize] = Some(vec![cells]);
                self.segs.insert(
                    at,
                    Seg { kind: SegKind::Ins { block }, len: 1 },
                );
                true
            }
            _ => false,
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
    Src { vpos: u64, rec: u32, snapshot: RowSnapshot, snapshot_row: u32 },
    Ins { vpos: u64, block: u32, row: u32, snapshot: RowSnapshot, snapshot_row: u32 },
    /// 保存后原 Source 已物化消失；撤销时恢复快照到预留块第 0 行。
    Suspended { vpos: u64, block: u32, snapshot: RowSnapshot, snapshot_row: u32 },
}

/// 保存前完成历史重建和块槽预留，替换目标后只做无失败移动。
struct PreparedSaveHistory {
    history: PreparedHistory<Op>,
    blocks: Vec<Option<Vec<Vec<String>>>>,
}

/// 临时文件阶段已验证的 backing/index；目标换名后只做无失败提交。
struct PreparedReopen {
    backing: Backing,
    idx: RawIndex,
    index_ms: u64,
    total_bytes: u64,
    modified: std::time::SystemTime,
    offset: u32,
    n_data: u32,
}

/// 单次删除的执行前段状态，仅在批量删除中途失败时用于回滚。
struct DeleteRollback {
    index: usize,
    off: u32,
    seg: Seg,
    removed_seg: bool,
    tombstone_was_present: bool,
    cells: Vec<String>,
}

// ============================================================
// 撤销（逆操作日志）
// ============================================================

/// 单元格稳定地址。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CellId {
    row: RowId,
    col: ColumnId,
}

/// 一个实际变化格子的批前/批后稀疏快照。
#[derive(Clone, PartialEq, Debug)]
struct CellChange {
    cell: CellId,
    old: ValueRef,
    new: ValueRef,
}

/// 一次清除操作的结构化目标；坐标均为未筛选的显示坐标。
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClearTarget {
    Cells { cells: Vec<(u64, u32)> },
    Rows { rows: Vec<u64> },
    Columns { cols: Vec<u32> },
    Rect { row: u64, col: u32, rows: u64, cols: u32 },
}

/// 排序去重并完成边界检查后的清除目标。
enum NormalizedClear {
    Cells { cells: Vec<(u64, u32)> },
    Rows { rows: Vec<u64> },
    Columns { cols: Vec<u32> },
    Rect { row: u64, col: u32, rows: u64, cols: u32 },
}

impl ClearTarget {
    /// 规范化结构目标，并在任何数据写入前完成 checked 边界检查。
    fn normalize(self, total_rows: u64, ncols: usize) -> Result<NormalizedClear, String> {
        match self {
            ClearTarget::Cells { mut cells } => {
                cells.sort_unstable();
                cells.dedup();
                Ok(NormalizedClear::Cells { cells })
            }
            ClearTarget::Rows { mut rows } => {
                if rows.iter().any(|&row| row >= total_rows) {
                    return Err("行号越界".into());
                }
                rows.sort_unstable();
                rows.dedup();
                Ok(NormalizedClear::Rows { rows })
            }
            ClearTarget::Columns { mut cols } => {
                if cols.iter().any(|&col| col as usize >= ncols) {
                    return Err("列号越界".into());
                }
                cols.sort_unstable();
                cols.dedup();
                Ok(NormalizedClear::Columns { cols })
            }
            ClearTarget::Rect { row, col, rows, cols } => {
                let row_end = row.checked_add(rows).ok_or("行号越界")?;
                if row_end > total_rows {
                    return Err("行号越界".into());
                }
                let col_end = (col as u64).checked_add(cols as u64).ok_or("列号越界")?;
                if col_end > ncols as u64 {
                    return Err("列号越界".into());
                }
                Ok(NormalizedClear::Rect { row, col, rows, cols })
            }
        }
    }
}

impl NormalizedClear {
    /// 按结构目标逐个访问坐标，不为整行、整列或矩形构造单元格 Vec。
    fn visit(
        &self,
        total_rows: u64,
        ncols: usize,
        mut visit: impl FnMut(u64, u32) -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            NormalizedClear::Cells { cells } => {
                for &(row, col) in cells {
                    visit(row, col)?;
                }
            }
            NormalizedClear::Rows { rows } => {
                for &row in rows {
                    for col in 0..ncols {
                        let col = u32::try_from(col).map_err(|_| "列号越界")?;
                        visit(row, col)?;
                    }
                }
            }
            NormalizedClear::Columns { cols } => {
                for row in 0..total_rows {
                    for &col in cols {
                        visit(row, col)?;
                    }
                }
            }
            NormalizedClear::Rect { row, col, rows, cols } => {
                for row_offset in 0..*rows {
                    for col_offset in 0..*cols {
                        let col = (*col)
                            .checked_add(col_offset)
                            .ok_or("列号越界")?;
                        visit(*row + row_offset, col)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// 历史中独立拥有的 UTF-8 文本；Arc 只共享实际文本分配。
#[derive(Debug, PartialEq)]
struct SharedText {
    text: Box<str>,
}

/// 历史值直接引用当前 backing 的源记录，或共享独立文本。
#[derive(Clone, PartialEq, Debug)]
enum ValueRef {
    Source {
        row: RowId,
        col: ColumnId,
        offset: u64,
        raw_len: u64,
        utf8_len: u64,
        index_epoch: u64,
    },
    Text(Arc<SharedText>),
}

impl ValueRef {
    fn text(value: String) -> Self {
        if value.is_empty() {
            static EMPTY: OnceLock<Arc<SharedText>> = OnceLock::new();
            let text = EMPTY.get_or_init(|| {
                Arc::new(SharedText { text: String::new().into_boxed_str() })
            });
            return Self::Text(Arc::clone(text));
        }
        Self::Text(Arc::new(SharedText { text: value.into_boxed_str() }))
    }

    fn add_own_cost(&self, cost: &mut Cost) -> Result<(), HistoryError> {
        if let Self::Source { utf8_len, .. } = self {
            cost.add(history_text_bytes(*utf8_len)?)?;
        }
        Ok(())
    }

    fn add_shared_charge(&self, charges: &mut Vec<SharedCharge>)
        -> Result<(), HistoryError> {
        if let Self::Text(text) = self {
            let bytes = history_text_bytes(text.text.len() as u64)?;
            charges.push(SharedCharge::of(text, bytes));
        }
        Ok(())
    }
}

/// 粘贴末尾新增行的最终内容及其实际插入块。
#[derive(Clone, PartialEq, Debug)]
struct PasteInsert {
    vpos: u64,
    block: u32,
    rows: Vec<Vec<String>>,
}

/// 前向操作（重做执行它，撤销执行其逆）。
#[derive(Clone, PartialEq, Debug)]
enum Op {
    SetCell { cell: CellId, old: ValueRef, new: ValueRef },
    SetCells { changes: Vec<CellChange> },
    Paste { changes: Vec<CellChange>, inserted: Option<PasteInsert> },
    InsertRows { vpos: u64, block: u32, rows: Vec<Vec<String>> },
    /// items 按 vpos 升序（重做按身份重新定位）。
    RemoveRows { items: Vec<DeletedRow> },
}

fn history_cost_error(error: HistoryError) -> String {
    format!("历史成本计算失败：{error:?}")
}

impl Op {
    fn rows_cost(cost: &mut Cost, rows: &Vec<Vec<String>>) -> Result<(), HistoryError> {
        cost.add_vec(rows)?;
        for row in rows {
            cost.add_vec(row)?;
            for value in row {
                cost.add_string(value)?;
            }
        }
        Ok(())
    }

    /// 单步完整保留成本：Vec 容量和 String 实际容量均计入。
    fn history_cost(&self) -> Result<u64, HistoryError> {
        let mut cost = Cost::new();
        cost.add_value::<Op>()?;
        match self {
            Op::SetCell { old, new, .. } => {
                old.add_own_cost(&mut cost)?;
                new.add_own_cost(&mut cost)?;
            }
            Op::SetCells { changes } => {
                cost.add_vec(changes)?;
                for change in changes {
                    change.old.add_own_cost(&mut cost)?;
                    change.new.add_own_cost(&mut cost)?;
                }
            }
            Op::Paste { changes, inserted } => {
                cost.add_vec(changes)?;
                for change in changes {
                    change.old.add_own_cost(&mut cost)?;
                    change.new.add_own_cost(&mut cost)?;
                }
                if let Some(inserted) = inserted {
                    Self::rows_cost(&mut cost, &inserted.rows)?;
                }
            }
            Op::InsertRows { rows, .. } => Self::rows_cost(&mut cost, rows)?,
            Op::RemoveRows { items } => {
                cost.add_vec(items)?;
            }
        }
        Ok(cost.bytes())
    }

    fn history_entry(self) -> Result<HistoryEntry<Op>, HistoryError> {
        let own_bytes = self.history_cost()?;
        let mut shared = Vec::new();
        match &self {
            Op::SetCell { old, new, .. } => {
                old.add_shared_charge(&mut shared)?;
                new.add_shared_charge(&mut shared)?;
            }
            Op::SetCells { changes } | Op::Paste { changes, .. } => {
                for change in changes {
                    change.old.add_shared_charge(&mut shared)?;
                    change.new.add_shared_charge(&mut shared)?;
                }
            }
            Op::RemoveRows { items } => {
                for item in items {
                    let snapshot = match item {
                        DeletedRow::Src { snapshot, .. }
                        | DeletedRow::Ins { snapshot, .. }
                        | DeletedRow::Suspended { snapshot, .. } => snapshot,
                    };
                    shared.push(snapshot.shared_charge());
                }
            }
            Op::InsertRows { .. } => {}
        }
        Ok(HistoryEntry::new(self, own_bytes, shared, 1))
    }
}

enum HistoryDecision {
    Normal(Prepared<Op>),
    FixedPoint(Prepared<Op>),
    Confirm(EditOutcome),
}

// ============================================================
// BigFile// ============================================================
// BigFile：大文件会话（mmap + 索引 + 编辑层）
// ============================================================

/// 待执行的列操作（渲染预览即时，物理重写在点击"执行"后进行）。
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ColOp {
    Insert { pos: u32, name: String },
    Delete { col: u32 },
    Rename { col: u32, name: String },
}

/// 筛选条件（与前端 grid.js _refilter 语义对齐：逐列"并且" +
/// 多重筛选勾选列"或"叠加；mode 0/1 的 needle 与 multi_text 均已小写化，
/// mode 2 关键字保持原样大小写）。
#[derive(Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FilterSpec {
    /// (列号, 关键字, 匹配模式)，列间 AND。
    /// 模式：0=包含、1=严格等于(不分大小写)、2=严格等于(区分大小写)。
    pub cols: Vec<(u32, String, u8)>,
    /// 多重筛选：勾选列 + 小写关键字（空串 = 未启用），列间 OR（恒为包含匹配）。
    pub multi_cols: Vec<u32>,
    pub multi_text: String,
}

/// 筛选命中页：行内容与各命中行的原始数据行号（0 基）成对下发。
/// vrows 与 rows 等长、逐行对齐；排序态经 orig_vrow 换算回原始行号（未排序恒等）。
#[derive(Clone, Default, serde::Serialize)]
pub struct FilterPage {
    pub rows: Vec<Vec<String>>,
    pub vrows: Vec<u32>,
}

/// 一次筛选的执行统计（测试/基准用：直扫 mmap，无任何列投影缓存）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterStats {
    /// 实际参与匹配的行数（前缀收窄时小于总行数；完全相同条件为 0）。
    pub scanned_rows: u64,
}

/// 保存是否落盘，或需要用户确认的固定点请求。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    pub saved: bool,
    pub fixed_point: Option<crate::history::FixedPointRequest>,
    pub cancelled: bool,
}

/// 打开后的元数据。
#[derive(Clone, serde::Serialize)]
pub struct BigMeta {
    pub rows: u64,
    pub cols: u32,
    pub headers: Vec<String>,
    /// 首行是否作为普通数据（无列名模式）。
    pub headerless: bool,
    pub encoding: String,
    pub pending_cols: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// 后台索引是否仍在进行（true 时 rows 会持续增长）。
    pub indexing: bool,
    /// 是否已载入私有内存缓冲（排序模式或新建内存档）。
    pub ram_backed: bool,
    /// 会话是否已关联磁盘路径（新建内存档为 false；前端据此决定保存走另存为）。
    pub has_path: bool,
    /// 当前是否处于排序视图（显示行按 `order` 映射）。
    pub sorted: bool,
    /// 是否存在上次保存后的内容修改（排序本身不算修改）。
    pub modified: bool,
    /// 已建立索引的字节数 / 文件总字节数。
    pub indexed_bytes: u64,
    pub total_bytes: u64,
    /// 首屏同步索引耗时（毫秒，从打开文件到返回前端）。
    pub open_ms: u64,
    /// 全量索引耗时（毫秒）；未完成时为 0。
    pub index_ms: u64,
    /// 全量索引是否在打开阶段同步完成（小文件）。此时 `index_ms` 与
    /// `open_ms` 是同一段端到端耗时，不是独立的索引计时，界面不应报数字。
    pub index_sync: bool,
    /// 距打开已过毫秒数（索引中实时增长）。
    pub elapsed_ms: u64,
}

/// 筛选开始时由视图列解析出的稳定身份与存储来源。
#[derive(Clone, Copy)]
enum ResolvedFilterColumn {
    Source { id: ColumnId, col: u32 },
    Inserted(ColumnId),
    Missing,
}

/// 已解析的筛选条件；Source 的 col 只用于本次原始字节扫描定位。
struct ResolvedFilterSpec {
    cols: Vec<(ResolvedFilterColumn, String, u8)>,
    multi_cols: Vec<ResolvedFilterColumn>,
    multi_text: String,
}

impl ResolvedFilterSpec {
    fn from_view(spec: &FilterSpec, schema: &Schema) -> Self {
        fn resolve(schema: &Schema, view_col: u32) -> ResolvedFilterColumn {
            let Some(id) = schema.view_id(view_col) else {
                return ResolvedFilterColumn::Missing;
            };
            match schema.storage(id) {
                Some(ColumnStorage::Source(col)) => ResolvedFilterColumn::Source { id, col },
                Some(ColumnStorage::Inserted(_)) => ResolvedFilterColumn::Inserted(id),
                None => ResolvedFilterColumn::Missing,
            }
        }
        Self {
            cols: spec
                .cols
                .iter()
                .map(|(col, text, mode)| (resolve(schema, *col), text.clone(), *mode))
                .collect(),
            multi_cols: spec.multi_cols.iter().map(|col| resolve(schema, *col)).collect(),
            multi_text: spec.multi_text.clone(),
        }
    }

    #[cfg(test)]
    fn identity_for_test(spec: &FilterSpec, ncols: u32) -> Self {
        fn resolve(view_col: u32, ncols: u32) -> ResolvedFilterColumn {
            if view_col < ncols {
                ResolvedFilterColumn::Source { id: ColumnId(view_col), col: view_col }
            } else {
                ResolvedFilterColumn::Missing
            }
        }
        Self {
            cols: spec
                .cols
                .iter()
                .map(|(col, text, mode)| (resolve(*col, ncols), text.clone(), *mode))
                .collect(),
            multi_cols: spec.multi_cols.iter().map(|col| resolve(*col, ncols)).collect(),
            multi_text: spec.multi_text.clone(),
        }
    }
}

/// 单条筛选关键字（mode 0/1 已小写；mode 2 保持原样大小写）。
struct Needle {
    col: u32,
    column: Option<ColumnId>,
    overlay_only: bool,
    text: String,
    /// 匹配模式：0 包含 / 1 严格等于·不分大小写 / 2 严格等于·区分大小写。
    mode: u8,
    ascii: bool,
    multi: bool,
    hit: bool,
}

/// 一次分块扫描的结果。
struct ChunkOut {
    hits: Vec<u32>,
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

/// 字节级字段扫描器：引号感知解析，逐字段匹配（不建任何投影缓存）。
struct FieldScan<'a> {
    enc: Encoding,
    overlay: &'a HashMap<(u32, ColumnId), String>,
    needles: Vec<Needle>,
    needle_by_col: HashMap<u32, Vec<usize>>,
    /// 列 → 位标记：1 = 参与匹配。
    col_flags: Vec<u8>,
    /// 字段原始内容（GBK 保留完整双字节对），供非 ASCII 匹配解码用。
    field_buf: Vec<u8>,
    /// 字段的 ASCII 可匹配字节：GBK 次字节不入内，避免次字节误中 ASCII 关键字。
    match_buf: Vec<u8>,
    /// 未折叠的字段原始字节：仅 mode 2（区分大小写）比较用，keep_raw 时才填充。
    raw_buf: Vec<u8>,
    /// 是否存在 mode 2 needle（无则 raw_buf 全程不填充，默认零开销）。
    keep_raw: bool,
    has_multi: bool,
    /// 快路径引号位缓冲（逐记录复用；无引号记录零分配）。
    quote_pos: Vec<u32>,
    /// 快路径引号区间表：扁平 (open, close) 对；close == 记录长表示未闭合。
    quote_iv: Vec<u32>,
}

impl<'a> FieldScan<'a> {
    /// 构造扫描器。
    ///
    /// # Parameters
    /// - `spec`: 已小写化的筛选条件。
    /// - `enc`: 文件编码（GBK 需要跳过双字节次字节）。
    /// - `overlay`: 单元格覆盖层（编辑值优先于原始字节）。
    /// - `ncols`: 物理列数。
    #[cfg(test)]
    fn new(
        spec: &FilterSpec,
        enc: Encoding,
        overlay: &'a HashMap<(u32, ColumnId), String>,
        ncols: usize,
    ) -> Self {
        let resolved = ResolvedFilterSpec::identity_for_test(spec, ncols as u32);
        Self::new_resolved(&resolved, enc, overlay, ncols)
    }

    fn new_resolved(
        spec: &ResolvedFilterSpec,
        enc: Encoding,
        overlay: &'a HashMap<(u32, ColumnId), String>,
        ncols: usize,
    ) -> Self {
        let mut needles = Vec::new();
        let mut needle_by_col: HashMap<u32, Vec<usize>> = HashMap::new();
        for (target, text, mode) in &spec.cols {
            let (col, column, overlay_only) = match *target {
                ResolvedFilterColumn::Source { id, col } => (col, Some(id), false),
                ResolvedFilterColumn::Inserted(id) => (0, Some(id), true),
                ResolvedFilterColumn::Missing => (0, None, false),
            };
            let index = needles.len();
            needles.push(Needle {
                col,
                column,
                overlay_only,
                text: text.clone(),
                mode: *mode,
                ascii: text.is_ascii(),
                multi: false,
                hit: false,
            });
            if let ResolvedFilterColumn::Source { col, .. } = *target {
                if (col as usize) < ncols {
                    needle_by_col.entry(col).or_default().push(index);
                }
            }
        }
        if !spec.multi_text.is_empty() {
            for target in &spec.multi_cols {
                let (col, column, overlay_only) = match *target {
                    ResolvedFilterColumn::Source { id, col } => (col, Some(id), false),
                    ResolvedFilterColumn::Inserted(id) => (0, Some(id), true),
                    ResolvedFilterColumn::Missing => (0, None, false),
                };
                let index = needles.len();
                needles.push(Needle {
                    col,
                    column,
                    overlay_only,
                    text: spec.multi_text.clone(),
                    mode: 0,
                    ascii: spec.multi_text.is_ascii(),
                    multi: true,
                    hit: false,
                });
                if let ResolvedFilterColumn::Source { col, .. } = *target {
                    if (col as usize) < ncols {
                        needle_by_col.entry(col).or_default().push(index);
                    }
                }
            }
        }
        let keep_raw = needles.iter().any(|needle| needle.mode == 2);
        let mut col_flags = vec![0u8; ncols];
        for col in needle_by_col.keys() {
            col_flags[*col as usize] |= 1;
        }
        Self {
            enc,
            overlay,
            needles,
            needle_by_col,
            col_flags,
            field_buf: Vec::new(),
            match_buf: Vec::new(),
            raw_buf: Vec::new(),
            keep_raw,
            has_multi: !spec.multi_text.is_empty(),
            quote_pos: Vec::new(),
            quote_iv: Vec::new(),
        }
    }

    /// 记录开始时重置命中，并解析没有原始物理槽的 Inserted 列。
    fn begin_record(&mut self, rec: u32) {
        for index in 0..self.needles.len() {
            self.needles[index].hit = false;
            if !self.needles[index].overlay_only {
                continue;
            }
            let hit = {
                let needle = &self.needles[index];
                let value = needle
                    .column
                    .and_then(|id| self.overlay.get(&(rec, id)))
                    .map(String::as_str)
                    .unwrap_or("");
                Self::value_hit(needle, value)
            };
            self.needles[index].hit = hit;
        }
    }

    /// 对已解码的稳定列值应用与原始字节扫描一致的匹配语义。
    fn value_hit(needle: &Needle, value: &str) -> bool {
        match needle.mode {
            2 => value == needle.text,
            1 if needle.ascii => value.to_ascii_lowercase() == needle.text,
            1 => value.to_lowercase() == needle.text,
            _ if needle.ascii => value.to_ascii_lowercase().contains(needle.text.as_str()),
            _ => value.to_lowercase().contains(needle.text.as_str()),
        }
    }

    /// 字段结束：匹配该列关键字（覆盖层优先）。
    /// 三态：0 包含（折叠子串）/ 1 严格等于·不分大小写（折叠整等）/
    /// 2 严格等于·区分大小写（比未折叠 raw_buf）。
    fn end_field(&mut self, rec: u32, col: u32) {
        let flag = self.col_flags.get(col as usize).copied().unwrap_or(0);
        if flag & 1 != 0 {
            if let Some(indices) = self.needle_by_col.get(&col) {
                for &index in indices {
                    if self.needles[index].hit {
                        continue;
                    }
                    let needle = &self.needles[index];
                    let overlay = needle.column.and_then(|id| self.overlay.get(&(rec, id)));
                    let hit = match overlay {
                        Some(value) => Self::value_hit(needle, value),
                        None => match needle.mode {
                            2 if needle.ascii => self.raw_buf.as_slice() == needle.text.as_bytes(),
                            2 => decode_slice(self.enc, &self.raw_buf) == needle.text,
                            1 if needle.ascii => self.match_buf.as_slice() == needle.text.as_bytes(),
                            1 => decode_slice(self.enc, &self.field_buf).to_lowercase() == needle.text,
                            _ if needle.ascii => {
                                memchr::memmem::find(&self.match_buf, needle.text.as_bytes()).is_some()
                            }
                            _ => decode_slice(self.enc, &self.field_buf)
                                .to_lowercase()
                                .contains(needle.text.as_str()),
                        },
                    };
                    self.needles[index].hit = hit;
                }
            }
        }
        self.field_buf.clear();
        self.match_buf.clear();
        self.raw_buf.clear();
    }

    /// 已扫描到第 `finalized_col` 列时，是否已可断定本行不匹配。
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
    ///
    /// 两级分发：GBK 且记录含非 ASCII 字节 → 标量状态机（双字节吞字节语义
    /// 逐字节依赖记录流，只有标量保证逐位一致）；其余 → memchr 驱动快路径。
    fn record_matches(&mut self, rec: u32, bytes: &[u8]) -> bool {
        if self.enc == Encoding::Gbk && !bytes.is_ascii() {
            self.record_matches_scalar(rec, bytes)
        } else {
            self.record_matches_fast(rec, bytes)
        }
    }

    /// 标量状态机（逐字节）。GBK 双字节记录的正确性基准；同时是快路径的
    /// 对拍参照——任何语义分歧以本函数为准。
    fn record_matches_scalar(&mut self, rec: u32, bytes: &[u8]) -> bool {
        self.begin_record(rec);
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
                                if self.keep_raw {
                                    self.raw_buf.push(b'"');
                                }
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
                            // 非 ASCII 匹配需要完整双字节；ASCII 匹配只放首字节，
                            // 否则次字节可能是 'a'..'z' 造成误中。
                            self.field_buf.push(b);
                            self.field_buf.push(bytes[i + 1]);
                            self.match_buf.push(b);
                            if self.keep_raw {
                                self.raw_buf.push(b);
                                self.raw_buf.push(bytes[i + 1]);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    if collect {
                        self.field_buf.push(b.to_ascii_lowercase());
                        self.match_buf.push(b.to_ascii_lowercase());
                        if self.keep_raw {
                            self.raw_buf.push(b);
                        }
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
                        if self.definitely_fails(col) {
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
                                if self.keep_raw {
                                    self.raw_buf.push(b);
                                    self.raw_buf.push(bytes[i + 1]);
                                }
                            }
                            i += 2;
                        } else {
                            if collect {
                                self.field_buf.push(b.to_ascii_lowercase());
                                self.match_buf.push(b.to_ascii_lowercase());
                                if self.keep_raw {
                                    self.raw_buf.push(b);
                                }
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

    /// 该列是否参与匹配（快路径对未参与列零拷贝跳过）。
    #[inline]
    fn participates(&self, col: u32) -> bool {
        (col as usize) < self.col_flags.len() && self.col_flags[col as usize] != 0
    }

    /// 测试用：该记录在两级分发中是否走快路径。
    #[cfg(test)]
    fn route_fast(&self, bytes: &[u8]) -> bool {
        !(self.enc == Encoding::Gbk && !bytes.is_ascii())
    }

    /// memchr 驱动快路径（UTF-8 全部 + GBK 纯 ASCII）：引号区间表 + SIMD 界符
    /// 搜索，未参与列零拷贝跳过。与 `record_matches_scalar` 语义逐位一致
    ///（特征化测试 + 固定种子随机对拍双保险）。
    fn record_matches_fast(&mut self, rec: u32, bytes: &[u8]) -> bool {
        self.begin_record(rec);
        let n = bytes.len();
        // 1) 引号位收集（缓冲逐记录复用；无引号记录首个 memchr 即 None，零分配）
        self.quote_pos.clear();
        let mut off = 0usize;
        while let Some(p) = memchr::memchr(b'"', &bytes[off..]) {
            self.quote_pos.push((off + p) as u32);
            off += p + 1;
        }
        // 2) 引号状态机 → 区间表（take 到局部变量，绕开后续 &mut self 借用）。
        //    转义对坍缩只在引号态内判定：非引号态的相邻对 = 开 + 关 = 净零切换，
        //    与标量逐位一致（奇数连引、未闭合截断由特征化测试钉住）。
        let mut iv = std::mem::take(&mut self.quote_iv);
        iv.clear();
        {
            let mut k = 0usize;
            let mut open = -1i64;
            while k < self.quote_pos.len() {
                let p = self.quote_pos[k] as usize;
                if open < 0 {
                    open = p as i64;
                    k += 1;
                } else if k + 1 < self.quote_pos.len() && self.quote_pos[k + 1] as usize == p + 1 {
                    k += 2; // 引号态内相邻对 = "" 转义，留在引号态
                } else {
                    iv.push(open as u32);
                    iv.push(p as u32);
                    open = -1;
                    k += 1;
                }
            }
            if open >= 0 {
                iv.push(open as u32);
                iv.push(n as u32); // 未闭合：吞到记录尾（标量同语义）
            }
        }
        // 3) 界符搜索主循环：memchr3（SIMD）找 , \r \n；落引号区间内的候选
        //    是内容，跳到闭引号之后续搜（区间游标单调推进，摊还 O(1)）。
        let mut field_start = 0usize;
        let mut col = 0u32;
        let mut pos = 0usize;
        let mut iv_i = 0usize;
        let result = loop {
            let b = match memchr::memchr3(b',', b'\r', b'\n', &bytes[pos..]) {
                Some(x) => pos + x,
                None => {
                    // 尾字段 [field_start, n)：末行无换行也按记录结束处理
                    if self.participates(col) {
                        assemble_field(
                            &mut self.field_buf,
                            &mut self.match_buf,
                            self.keep_raw.then_some(&mut self.raw_buf),
                            bytes,
                            field_start,
                            n,
                            &iv,
                        );
                        self.end_field(rec, col);
                    }
                    break self.record_result();
                }
            };
            while iv_i + 1 < iv.len() && (iv[iv_i + 1] as usize) < b {
                iv_i += 2; // 区间整体在候选之前，过
            }
            if iv_i + 1 < iv.len() {
                let (o, c) = (iv[iv_i] as usize, iv[iv_i + 1] as usize);
                if o < b && b < c {
                    pos = (c + 1).min(n); // 候选是引号内界符：跳过本区间
                    continue;
                }
            }
            // 真实界符
            if self.participates(col) {
                assemble_field(
                    &mut self.field_buf,
                    &mut self.match_buf,
                    self.keep_raw.then_some(&mut self.raw_buf),
                    bytes,
                    field_start,
                    b,
                    &iv,
                );
                self.end_field(rec, col);
            }
            if self.definitely_fails(col) {
                break false;
            }
            col += 1;
            if bytes[b] == b',' {
                field_start = b + 1;
                pos = b + 1;
            } else {
                break self.record_result(); // \r 或 \n：记录结束
            }
        };
        self.quote_iv = iv;
        result
    }
}

/// 把 bytes[fs..fe] 的字段内容按标量语义组装进两个缓冲：引号剥离、
/// "" 对坍缩成单引号、ASCII 逐字节折叠。仅用于 UTF-8 / 纯 ASCII 记录
///（无 GBK 吞字节）；两缓冲内容相同（与标量非 GBK 分支逐位一致）。
///
/// # Parameters
/// - `field_buf`/`match_buf`: 目标缓冲（追加写入，清空由 end_field 负责）。
/// - `raw_buf`: Some 时追加写入折叠前的原始字节（mode 2 比较用）。
/// - `bytes`: 整条记录原始字节。
/// - `fs`/`fe`: 字段字节区间（含引号字符）。
/// - `iv`: 扁平 (open, close) 引号区间表；close == 记录长表示未闭合。
fn assemble_field(
    field_buf: &mut Vec<u8>,
    match_buf: &mut Vec<u8>,
    raw_buf: Option<&mut Vec<u8>>,
    bytes: &[u8],
    fs: usize,
    fe: usize,
    iv: &[u32],
) {
    let base = field_buf.len();
    let mut cur = fs;
    let mut k = 0usize;
    // 界符必在引号外，故区间不会跨过字段界：要么内包于字段，要么整体在外
    while k + 1 < iv.len() {
        let (o, c) = (iv[k] as usize, iv[k + 1] as usize);
        k += 2;
        if c < fs {
            continue;
        }
        if o >= fe {
            break;
        }
        debug_assert!(o >= fs, "引号区间不应跨过字段界");
        if cur < o {
            field_buf.extend_from_slice(&bytes[cur..o]);
        }
        // 引号区内容 (o+1..c)：区间内的引号必为转义对，坍缩成单引号
        let mut q = o + 1;
        let qend = c.min(fe);
        while q < qend {
            match memchr::memchr(b'"', &bytes[q..qend]) {
                Some(p) => {
                    field_buf.extend_from_slice(&bytes[q..q + p]);
                    field_buf.push(b'"');
                    q += p + 2;
                }
                None => {
                    field_buf.extend_from_slice(&bytes[q..qend]);
                    break;
                }
            }
        }
        cur = c + 1; // 闭引号不入内容
    }
    if cur < fe {
        field_buf.extend_from_slice(&bytes[cur..fe]);
    }
    if let Some(rb) = raw_buf {
        rb.extend_from_slice(&field_buf[base..]); // 折叠前的原始字节（mode 2 用）
    }
    field_buf[base..].make_ascii_lowercase();
    match_buf.extend_from_slice(&field_buf[base..]);
}

/// 新条件能否在旧命中集上收窄：列结构一致，且每个关键字都以前一版为前缀。
fn can_narrow(prev: &FilterSpec, next: &FilterSpec) -> bool {
    // 严格等于破坏前缀单调（等于("ab") 与等于("abc") 不相交）：
    // 任一侧含严格列即不可收窄（严格列上换关键字 = 全量重扫）。
    if prev.cols.iter().any(|c| c.2 != 0) || next.cols.iter().any(|c| c.2 != 0) {
        return false;
    }
    if prev.cols.len() != next.cols.len() {
        return false;
    }
    let mut pc: Vec<(u32, &str)> = prev.cols.iter().map(|(c, k, _)| (*c, k.as_str())).collect();
    let mut nc: Vec<(u32, &str)> = next.cols.iter().map(|(c, k, _)| (*c, k.as_str())).collect();
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

/// 会话字节后备：常规浏览用只读文件映射；排序模式载入后换成私有内存缓冲。
///
/// 读路径一律经 [`BigFile::bytes`] 取字节，不直接假设后备类型——这样
/// 排序模式（内存）与普通模式（mmap）共用索引、筛选、编辑与保存逻辑。
enum Backing {
    /// 文件映射：后台索引线程也持有一份 Arc，读路径不复制文件内容。
    Mmap(Arc<memmap2::Mmap>),
    /// 私有内存缓冲：与磁盘映射解耦后可释放映射，稳定态约 1× 文件大小。
    Ram(Vec<u8>),
}

impl Backing {
    /// 当前后备的只读字节视图。
    fn bytes(&self) -> &[u8] {
        match self {
            Backing::Mmap(m) => m,
            Backing::Ram(v) => v,
        }
    }

    /// 是否已切换为私有内存缓冲。
    fn is_ram(&self) -> bool {
        matches!(self, Backing::Ram(_))
    }
}

/// 排序模式内存估算（弹窗展示用；单位字节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SortEstimate {
    /// 文件字节数（已载入内存时仍报告原文件大小）。
    pub file_bytes: u64,
    /// 数据行数（不含表头）。
    pub row_count: u64,
    /// 预计新增私有内存：文件缓冲（未载入时）+ 排列 + 键 + 排序 scratch + 余量。
    pub new_bytes: u64,
    /// 载入瞬间的物理峰值提示（私有缓冲 + 可回收的文件页缓存）。
    pub peak_bytes: u64,
    /// 当前系统可用物理内存；平台不支持时为 null。
    pub available_bytes: Option<u64>,
}

/// 单个排序级别（语义对齐小模式 `sortRows` 的 [col, mode, asc]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SortLevel {
    /// 视图列号。
    pub col: u32,
    /// true = 按数字比较（非数字恒排最后）；false = 按文本比较。
    pub numeric: bool,
    /// true = 升序。
    pub asc: bool,
}

/// 一级排序键（以旧显示位置为索引）。
enum SortKeys {
    /// 数字键：NaN 表示非数字（恒排最后，与升降序无关）。
    Num(Vec<f64>),
    /// 文本键：UTF-8 字节池 + 每行的起止偏移。
    Text { blob: Vec<u8>, offs: Vec<u64> },
}

impl SortKeys {
    /// 比较两个位置在本级键上的顺序。
    ///
    /// # Parameters
    /// - `a` / `b`: 旧显示位置。
    /// - `asc`: true = 升序；数字非数字始终排在最后。
    ///
    /// # Returns
    /// 比较结果（相等时由调用方进入下一级）。
    fn cmp_at(&self, a: usize, b: usize, asc: bool) -> CmpOrdering {
        match self {
            SortKeys::Num(v) => {
                let (x, y) = (v[a], v[b]);
                match (x.is_nan(), y.is_nan()) {
                    (true, true) => CmpOrdering::Equal,
                    (true, false) => CmpOrdering::Greater,
                    (false, true) => CmpOrdering::Less,
                    (false, false) => {
                        let o = x.partial_cmp(&y).unwrap_or(CmpOrdering::Equal);
                        if asc { o } else { o.reverse() }
                    }
                }
            }
            SortKeys::Text { blob, offs } => {
                let x = &blob[offs[a] as usize..offs[a + 1] as usize];
                let y = &blob[offs[b] as usize..offs[b + 1] as usize];
                let o = x.cmp(y);
                if asc { o } else { o.reverse() }
            }
        }
    }
}

/// 按 JavaScript `Number()` + `Number.isFinite` 语义解析数字。
///
/// 与小模式 `toFloat` 一致：trim、空串/非有限值视为非数字；
/// 支持 `0x`/`0b`/`0o` 前缀与前导 `+`。
///
/// # Parameters
/// - `s`: 单元格原始文本。
///
/// # Returns
/// 有限 `f64`；无法解析或非有限时为 None。
fn js_number_parse(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let parse_radix = |digits: &str, radix: u32| -> Option<f64> {
        if digits.is_empty() {
            return None;
        }
        let mut v = 0.0f64;
        for ch in digits.chars() {
            let d = ch.to_digit(radix)?;
            v = v * radix as f64 + d as f64;
        }
        Some(v)
    };
    let parsed = if let Some(d) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        parse_radix(d, 16)
    } else if let Some(d) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        parse_radix(d, 2)
    } else if let Some(d) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        parse_radix(d, 8)
    } else {
        // JS 允许前导 '+'；Rust 的 f64 解析不接受。
        t.strip_prefix('+').unwrap_or(t).parse::<f64>().ok()
    };
    parsed.filter(|v| v.is_finite())
}

/// 查询当前系统可用物理内存（字节）。
///
/// Windows 用 `GlobalMemoryStatusEx`，macOS 用 `sysctlbyname` 汇总空闲/非活跃页，
/// Linux 读 `/proc/meminfo` 的 `MemAvailable`；其余平台返回 None。
///
/// # Returns
/// 可用字节数；查询失败或平台不支持时为 None。
pub fn available_memory_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        platform_mem::windows_available()
    }
    #[cfg(target_os = "macos")]
    {
        platform_mem::macos_available()
    }
    #[cfg(target_os = "linux")]
    {
        platform_mem::linux_available()
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// 各平台的可用内存查询实现（不引入额外 crate，直接 FFI/读 proc）。
mod platform_mem {
    /// 把 sysctl 返回的原生整数字节解码为 `u64`。
    ///
    /// macOS 的 sysctl 宽度并不统一：`vm.page_free_count` 等计数器是
    /// 4 字节 `uint32`，而 `hw.pagesize` 在 Apple Silicon 上经
    /// `sysctlbyname` 可能返回 8 字节。调用方按实际回填长度传入切片。
    ///
    /// # Parameters
    /// - `bytes`: sysctl 实际写入的字节（4 或 8 字节）。
    ///
    /// # Returns
    /// 解码后的值；长度不是 4/8 时返回 None。
    #[cfg(any(target_os = "macos", test))]
    fn decode_native_uint(bytes: &[u8]) -> Option<u64> {
        match bytes.len() {
            4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
            8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
            _ => None,
        }
    }

    /// Windows：GlobalMemoryStatusEx.ullAvailPhys。
    #[cfg(windows)]
    pub fn windows_available() -> Option<u64> {
        #[repr(C)]
        struct MemoryStatusEx {
            length: u32,
            memory_load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page_file: u64,
            avail_page_file: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended_virtual: u64,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        }

        // SAFETY: 结构体按 Win32 布局定义，length 为约定值，调用只写该缓冲。
        let mut status: MemoryStatusEx = unsafe { std::mem::zeroed() };
        status.length = std::mem::size_of::<MemoryStatusEx>() as u32;
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0).then_some(status.avail_phys)
    }

    /// macOS：空闲页 + 非活跃页（可回收）折算字节。
    #[cfg(target_os = "macos")]
    pub fn macos_available() -> Option<u64> {
        use std::os::raw::{c_char, c_int, c_void};

        extern "C" {
            fn sysctlbyname(
                name: *const c_char,
                oldp: *mut c_void,
                oldlenp: *mut usize,
                newp: *mut c_void,
                newlen: usize,
            ) -> c_int;
        }

        fn read_uint(name: &str) -> Option<u64> {
            let cname = std::ffi::CString::new(name).ok()?;
            // 缓冲按最大宽度 8 字节准备；sysctl 会回填实际宽度（vm.* 计数为 4 字节，
            // hw.pagesize 在 Apple Silicon 上可能为 8 字节），再按 len 解码。
            let mut buf = [0u8; std::mem::size_of::<u64>()];
            let mut len = buf.len();
            // SAFETY: 缓冲区与长度均指向本栈变量，name 为有效 C 串。
            let rc = unsafe {
                sysctlbyname(
                    cname.as_ptr(),
                    buf.as_mut_ptr() as *mut c_void,
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 || len > buf.len() {
                return None;
            }
            decode_native_uint(&buf[..len])
        }

        let free = read_uint("vm.page_free_count")?;
        let inactive = read_uint("vm.page_inactive_count").unwrap_or(0);
        let page = read_uint("hw.pagesize").unwrap_or(4096);
        Some(free.saturating_add(inactive).saturating_mul(page))
    }

    /// Linux：/proc/meminfo 的 MemAvailable。
    #[cfg(target_os = "linux")]
    pub fn linux_available() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
                return Some(kb.saturating_mul(1024));
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::decode_native_uint;

        #[test]
        fn decodes_4_and_8_byte_native_ints() {
            let four = 1234u32.to_ne_bytes();
            assert_eq!(decode_native_uint(&four), Some(1234));
            let eight = 12_345_678_901u64.to_ne_bytes();
            assert_eq!(decode_native_uint(&eight), Some(12_345_678_901));
        }

        #[test]
        fn rejects_other_widths() {
            assert_eq!(decode_native_uint(&[]), None);
            assert_eq!(decode_native_uint(&[1, 2, 3]), None);
            assert_eq!(decode_native_uint(&[0; 16]), None);
        }
    }
}

static NEXT_SESSION_SPOOL: AtomicU64 = AtomicU64::new(0);

fn new_session_spool() -> Result<SessionSpool, String> {
    let id = NEXT_SESSION_SPOOL.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("simpletable-{}-{id}", std::process::id()));
    SessionSpool::create(&root)
}

/// 大文件编辑会话。
pub struct BigFile {
    path: PathBuf,
    /// Task 8/14 重绑定和固定点 generation 使用；与用户保存身份 path 分离。
    #[allow(dead_code)]
    backing_path: PathBuf,
    /// 字节后备：常规为文件映射，排序模式载入后为私有内存缓冲。
    backing: Backing,
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
    /// 全量索引是否在打开阶段同步完成（无后台线程）。
    index_sync: bool,
    /// 文件总字节数（meta 展示进度用）。
    total_bytes: u64,
    /// 仅表示原始记录中的物理源列数，不承担历史身份。
    base_ncols: usize,
    base_headers: Vec<String>,
    /// 已提交逻辑结构；历史与 pending 预览共享不可变列级对象。
    schema: Arc<Schema>,
    pending_schema: Option<Arc<Schema>>,
    seg: Segments,
    /// 源记录编辑层：(稳定记录号, 稳定列身份) → 新值。
    overlay: HashMap<(u32, ColumnId), String>,
    pending_cols: Vec<ColOp>,
    filter: Option<Vec<u32>>,
    /// 上次完成的筛选条件（前缀收窄 / 完全相同条件直接返回）。
    last_spec: Option<FilterSpec>,
    filter_stats: FilterStats,
    history: HistoryLedger<Op>,
    /// 删除快照和后续固定点 generation 的会话独占临时目录。
    spool: SessionSpool,
    history_generation: u64,
    /// backing 与索引替换代际；Source 历史值只能读取同一代。
    index_epoch: u64,
    fixed_point: bool,
    savepoint_detached: bool,
    base_size: u64,
    base_mtime: std::time::SystemTime,
    /// 原文件是否带 UTF-8 BOM：保存/列操作重写时按原样回写（不注入也不剥除）。
    has_bom: bool,
    /// 无列名模式：记录 0 也是数据，列名为虚拟的 列1..N。
    headerless: bool,
    /// 排序排列：显示位置 → 原始 vrow；None = 未排序。
    order: Option<Vec<u32>>,
    /// 内容修改计数：每次单元格/行结构变化递增，保存后归零。
    /// 排序只改变视图，不递增。
    revision: i64,
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
    #[cfg(test)]
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with(path, false)
    }

    /// 按指定首行语义打开：`headerless = true` 时记录 0 是普通数据。
    ///
    /// # Parameters
    /// - `path`: CSV 文件路径。
    /// - `headerless`: true = 首行不是列名（列名虚拟为 列1..N）。
    ///
    /// # Errors
    /// 文件不可读/为空/无记录时返回错误字符串。
    pub fn open_with(path: &Path, headerless: bool) -> Result<Self, String> {
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
        Self::from_parts(
            path.to_path_buf(),
            Backing::Mmap(mmap),
            meta.len(),
            meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            headerless,
            t0,
        )
    }

    /// 从内存字节直接建会话（新建表格：不写盘、无路径）。
    ///
    /// 与磁盘打开共用 from_parts 管线；会话以 Ram 后备持有字节且
    /// has_path() == false——save 拒绝直存，save_as 落盘后自动获得路径。
    ///
    /// # Errors
    /// 缓冲为空或无记录时返回错误字符串（与磁盘打开同语义）。
    pub fn from_bytes(buf: Vec<u8>, headerless: bool) -> Result<Self, String> {
        let t0 = Instant::now();
        if buf.is_empty() {
            return Err("空文件".into());
        }
        let total = buf.len() as u64;
        Self::from_parts(
            PathBuf::new(),
            Backing::Ram(buf),
            total,
            std::time::SystemTime::UNIX_EPOCH,
            headerless,
            t0,
        )
    }

    /// 打开管线共用收尾：嗅探编码 → 全量索引 → 首记录解析 → 字段装配。
    /// 整开（mmap）与内存建表（Ram）仅差后备来源与路径/元数据。
    fn from_parts(
        path: PathBuf,
        backing: Backing,
        total_bytes: u64,
        base_mtime: std::time::SystemTime,
        headerless: bool,
        t0: Instant,
    ) -> Result<Self, String> {
        let bytes: &[u8] = backing.bytes();
        let enc = sniff_encoding(bytes);
        // BOM 只对 UTF-8 有意义（GBK 文件可能以相同字节开头纯属巧合）
        let has_bom = enc == Encoding::Utf8 && bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let idx = build_index(bytes);
        if idx.n_records() == 0 {
            return Err("文件中没有记录".into());
        }
        let (hs, he) = idx.record_span(bytes.len(), 0);
        let mut first_fields = parse_record(&decode_slice(enc, &bytes[hs..he]), b',');
        if enc == Encoding::Utf8 {
            if let Some(first) = first_fields.first_mut() {
                if let Some(stripped) = first.strip_prefix('\u{FEFF}') {
                    *first = stripped.to_string();
                }
            }
        }
        // 有表头：记录 0 是列名，数据从物理记录 1 开始；
        // 无列名：记录 0 也是数据，列数由它决定。
        let offset = if headerless { 0u32 } else { 1u32 };
        let n_data = idx.n_records() - offset as usize;
        let headers = if headerless {
            generic_headers(first_fields.len())
        } else {
            first_fields
        };
        let base_ncols = headers.len();
        let schema = Arc::new(Schema::from_headers(headers.clone())?);
        let open_ms = t0.elapsed().as_millis() as u64;
        let spool = new_session_spool()?;
        Ok(Self {
            backing_path: path.clone(),
            path,
            backing,
            enc,
            idx,
            partial: None,
            cancel: Arc::new(AtomicBool::new(false)),
            open_started: t0,
            open_ms,
            index_ms: open_ms,
            index_sync: true,
            total_bytes,
            base_ncols,
            base_headers: headers,
            schema,
            pending_schema: None,
            seg: Segments::new(offset, n_data as u32),
            overlay: HashMap::new(),
            pending_cols: Vec::new(),
            filter: None,
            last_spec: None,
            filter_stats: FilterStats::default(),
            history: HistoryLedger::new(DEFAULT_HISTORY_BUDGET_BYTES),
            spool,
            history_generation: 0,
            index_epoch: 0,
            fixed_point: false,
            savepoint_detached: false,
            base_size: total_bytes,
            base_mtime,
            has_bom,
            headerless,
            order: None,
            revision: 0,
        })
    }

    /// 按指定首行语义渐进打开。
    ///
    /// # Parameters
    /// - `path`: CSV 文件路径。
    /// - `headerless`: true = 首行不是列名。
    ///
    /// # Errors
    /// 文件不可读/为空/首条记录无法完成时返回错误字符串。
    pub fn open_progressive_with_mode(
        path: &Path,
        headerless: bool,
    ) -> Result<Self, String> {
        Self::open_progressive_full(
            path,
            INITIAL_INDEX_BYTES,
            INITIAL_INDEX_RECORDS,
            INDEX_CHUNK_BYTES,
            true,
            headerless,
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
    #[cfg(test)]
    fn open_progressive_with(
        path: &Path,
        initial_bytes: usize,
        initial_records: usize,
        chunk_bytes: usize,
        background: bool,
    ) -> Result<Self, String> {
        Self::open_progressive_full(
            path,
            initial_bytes,
            initial_records,
            chunk_bytes,
            background,
            false,
        )
    }

    /// 渐进打开完整参数版（含首行语义）。
    fn open_progressive_full(
        path: &Path,
        initial_bytes: usize,
        initial_records: usize,
        chunk_bytes: usize,
        background: bool,
        headerless: bool,
    ) -> Result<Self, String> {
        let t0 = Instant::now();
        let spool = new_session_spool()?;
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
        let mut first_fields =
            parse_record(&decode_slice(enc, &bytes[header_start..header_end]), b',');
        if enc == Encoding::Utf8 {
            if let Some(first) = first_fields.first_mut() {
                if let Some(stripped) = first.strip_prefix('\u{FEFF}') {
                    *first = stripped.to_string();
                }
            }
        }
        let offset = if headerless { 0usize } else { 1usize };
        let n_data = usable - offset;
        let headers = if headerless {
            generic_headers(first_fields.len())
        } else {
            first_fields
        };
        let base_ncols = headers.len();
        let schema = Arc::new(Schema::from_headers(headers.clone())?);
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
            backing_path: path.to_path_buf(),
            backing: Backing::Mmap(mmap),
            enc,
            idx,
            partial,
            cancel,
            open_started: t0,
            open_ms,
            index_ms,
            index_sync: done,
            total_bytes,
            base_ncols,
            base_headers: headers,
            schema,
            pending_schema: None,
            seg: Segments::new(offset as u32, n_data as u32),
            overlay: HashMap::new(),
            pending_cols: Vec::new(),
            filter: None,
            last_spec: None,
            filter_stats: FilterStats::default(),
            history: HistoryLedger::new(DEFAULT_HISTORY_BUDGET_BYTES),
            spool,
            history_generation: 0,
            index_epoch: 0,
            fixed_point: false,
            savepoint_detached: false,
            base_size: meta.len(),
            base_mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            has_bom,
            headerless,
            order: None,
            revision: 0,
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
        let bytes: &[u8] = self.bytes();
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
        // 磁盘索引按物理记录号解释：有表头跳过记录 0，无列名从记录 0 开始。
        // 注意模式可能在索引期间切换过，这里用当前开关状态重算。
        let offset = self.record_offset() as usize;
        let n_data = rec_lines.len().saturating_sub(offset);
        self.idx = RawIndex { line_off, rec_lines };
        self.seg = Segments::new(offset as u32, n_data as u32);
        self.index_ms = index_ms;
        self.partial = None;
    }

    /// 视图行 → 物理记录号的模式偏移。
    ///
    /// 仅用于重新解释磁盘索引（打开/并入索引/渐进期换算）；已建好的
    /// 段表一律按物理记录号解析，不能再叠加此偏移。
    fn record_offset(&self) -> u32 {
        if self.headerless {
            0
        } else {
            1
        }
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
            .saturating_sub(self.record_offset() as usize) as u64
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
            Some((start, end.min(self.bytes().len())))
        } else {
            if r >= self.idx.n_records() {
                return None;
            }
            Some(self.idx.record_span(self.bytes().len(), r))
        }
    }

    /// 当前后备的只读字节视图（mmap 或内存缓冲）。
    fn bytes(&self) -> &[u8] {
        self.backing.bytes()
    }

    /// 是否已切换到内存后备（排序模式或新建内存档）。
    pub fn is_ram_backed(&self) -> bool {
        self.backing.is_ram()
    }

    /// 会话是否已关联磁盘路径（新建内存档为 false，另存为后转 true）。
    pub fn has_path(&self) -> bool {
        !self.path.as_os_str().is_empty()
    }

    /// 当前是否处于排序视图。
    pub fn is_sorted(&self) -> bool {
        self.order.is_some()
    }

    /// 显示位置 → 原始 vrow（未排序时恒等）。
    ///
    /// # Parameters
    /// - `display`: 当前视图行号。
    ///
    /// # Returns
    /// 排序前的稳定行号；排列越界时原样返回（调用方会再判越界）。
    fn orig_vrow(&self, display: u64) -> u64 {
        match &self.order {
            Some(order) => order
                .get(display as usize)
                .copied()
                .map_or(display, |v| v as u64),
            None => display,
        }
    }

    /// 按原始 vrow 读取字段（键构建用；不做显示映射）。
    fn orig_fields(&self, vrow: u64) -> Vec<String> {
        match self.seg.resolve(vrow) {
            Some(ResolvedRow::Src(rec)) => self.src_fields(rec),
            Some(ResolvedRow::Ins { block, row }) => self.ins_fields(block, row),
            None => vec![String::new(); self.schema.visible().len()],
        }
    }

    /// 按级别对当前视图排序（排列层，不改文件；需要内存后备）。
    ///
    /// 语义对齐小模式：多级依次比较；数字级非数字恒排最后；
    /// 文本按 UTF-8 字节序；完全相等时保持原显示顺序（稳定）。
    /// 已激活的筛选结果会换算到新的显示位置，成员不变。
    ///
    /// # Parameters
    /// - `levels`: 排序级别（列号按当前视图列号）。
    ///
    /// # Errors
    /// 未载入内存 / 索引未完成 / 待执行列操作 / 级别为空或列越界时返回错误字符串。
    pub fn sort_by_levels(&mut self, levels: &[SortLevel]) -> Result<(), String> {
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持排序".into());
        }
        if !self.is_ram_backed() {
            return Err("排序需要先把文件载入内存".into());
        }
        if levels.is_empty() {
            return Err("排序级别不能为空".into());
        }
        let ncols = self.view_ncols();
        if let Some(l) = levels.iter().find(|l| l.col >= ncols) {
            return Err(format!("排序列越界：{}（共 {} 列）", l.col, ncols));
        }
        let sort_columns: Vec<(ColumnId, usize)> = levels
            .iter()
            .map(|level| {
                let id = self.schema.view_id(level.col).ok_or("排序列越界")?;
                let position = self
                    .schema
                    .visible()
                    .iter()
                    .position(|candidate| *candidate == id)
                    .ok_or("排序列越界")?;
                Ok((id, position))
            })
            .collect::<Result<_, String>>()?;
        let total = self.seg.vrow_count();
        if total > u32::MAX as u64 {
            return Err("文件过大（超过 42.9 亿行），暂不支持排序".into());
        }
        let total = total as usize;
        // 旧显示序：新排列以它为“原始顺序”，保证稳定与命中集换算。
        let old_order: Vec<u32> = self
            .order
            .clone()
            .unwrap_or_else(|| (0..total as u32).collect());

        // 逐级建键：一次遍历取一行；键按旧显示位置索引。
        let mut keys: Vec<SortKeys> = Vec::with_capacity(levels.len());
        for (level, (_, position)) in levels.iter().zip(&sort_columns) {
            let mut out = if level.numeric {
                SortKeys::Num(Vec::with_capacity(total))
            } else {
                SortKeys::Text {
                    blob: Vec::new(),
                    offs: Vec::with_capacity(total + 1),
                }
            };
            if let SortKeys::Text { offs, .. } = &mut out {
                offs.push(0);
            }
            for &orig in &old_order {
                let fields = self.orig_fields(orig as u64);
                let text = fields.get(*position).map(String::as_str).unwrap_or("");
                match &mut out {
                    SortKeys::Num(v) => {
                        v.push(js_number_parse(text).unwrap_or(f64::NAN));
                    }
                    SortKeys::Text { blob, offs } => {
                        blob.extend_from_slice(text.as_bytes());
                        offs.push(blob.len() as u64);
                    }
                }
            }
            keys.push(out);
        }

        // 排列：perm[新位置] = 旧显示位置；平局用旧显示序，等价稳定排序。
        let mut perm: Vec<u32> = (0..total as u32).collect();
        perm.sort_unstable_by(|&a, &b| {
            let (ai, bi) = (a as usize, b as usize);
            for (i, level) in levels.iter().enumerate() {
                let o = keys[i].cmp_at(ai, bi, level.asc);
                if o != CmpOrdering::Equal {
                    return o;
                }
            }
            a.cmp(&b)
        });

        let mut new_order: Vec<u32> = Vec::with_capacity(total);
        let mut rank: Vec<u32> = vec![0; total];
        for (d, &old_d) in perm.iter().enumerate() {
            let orig = old_order[old_d as usize];
            new_order.push(orig);
            rank[orig as usize] = d as u32;
        }

        // 命中集存的是显示位置：换算并保持升序（= 新视图的浏览顺序）。
        if let Some(hits) = self.filter.take() {
            let mut mapped: Vec<u32> = hits
                .iter()
                .map(|&d| rank[old_order[d as usize] as usize])
                .collect();
            mapped.sort_unstable();
            self.filter = Some(mapped);
        }
        let next_history_generation = self.next_history_generation()?;
        self.order = Some(new_order);
        // 当前排序实现仍截断历史；共享排列历史由后续任务接入。
        self.clear_history_at_fixed_point(next_history_generation);
        Ok(())
    }

    /// 退出排序视图；命中集换算回原始行号。
    ///
    /// # Parameters
    /// - `release_ram`: true = 同时释放私有内存缓冲，重新映射原文件。
    ///
    /// # Errors
    /// 重新映射失败时返回错误字符串（此时已退出排序视图）。
    pub fn exit_sort_mode(&mut self, release_ram: bool) -> Result<(), String> {
        if let Some(order) = self.order.take() {
            if let Some(hits) = self.filter.take() {
                let mut mapped: Vec<u32> = hits
                    .iter()
                    .map(|&d| order.get(d as usize).copied().unwrap_or(d))
                    .collect();
                mapped.sort_unstable();
                self.filter = Some(mapped);
            }
        }
        // 无路径内存档没有可回落的磁盘原文件：保持 Ram 后备。
        if release_ram && self.is_ram_backed() && self.has_path() {
            // 文件本身未变（编辑都在覆盖层），可直接重建映射与索引。
            self.remap()?;
        }
        Ok(())
    }

    /// 估算排序模式需要的新增内存（不含磁盘页缓存）。
    ///
    /// # Parameters
    /// - `levels`: 排序级别数（≥1；每级需要独立的键数组）。
    ///
    /// # Returns
    /// 文件字节、行数、预计新增/峰值字节与系统可用内存。
    pub fn sort_estimate(&self, levels: u32) -> SortEstimate {
        let file_bytes = self.bytes().len() as u64;
        let rows = if self.partial.is_some() {
            self.indexed_data_rows()
        } else {
            self.seg.vrow_count()
        };
        let levels = levels.max(1) as u64;
        // 排列 4 B/行；键 8 B/行/级（数字）或近似一行字节（文本键）。
        let perm = rows.saturating_mul(4);
        let avg_row = if rows > 0 { file_bytes / rows } else { 0 };
        // 文本键按整行字节上界估算；GBK 解码成 UTF-8 最多约 1.5×。
        let per_level = avg_row
            .saturating_mul(if self.enc == Encoding::Gbk { 2 } else { 1 })
            .max(8);
        let keys = rows.saturating_mul(per_level).saturating_mul(levels);
        let scratch = rows.saturating_mul(16);
        // 已载入内存时不再重复计算文件缓冲。
        let buffer = if self.is_ram_backed() { 0 } else { file_bytes };
        let base = buffer
            .saturating_add(perm)
            .saturating_add(keys)
            .saturating_add(scratch);
        let margin = base / 10 + 32 * 1024 * 1024;
        let new_bytes = base.saturating_add(margin);
        // 载入瞬间私有缓冲与文件页缓存并存，物理峰值约 2×（页缓存可回收）。
        let peak_bytes = if self.is_ram_backed() {
            new_bytes
        } else {
            file_bytes.saturating_mul(2).max(new_bytes)
        };
        SortEstimate {
            file_bytes,
            row_count: rows,
            new_bytes,
            peak_bytes,
            available_bytes: available_memory_bytes(),
        }
    }

    /// 把文件内容分块复制到私有内存缓冲；完成后释放文件映射。
    ///
    /// 复制期间每块检查 `cancel`：取消只丢弃半成品缓冲，原 mmap 视图原样保留，
    /// 不改文件、不标脏。索引未完成时拒绝（排序键需要完整行索引）。
    ///
    /// # Parameters
    /// - `cancel`: 取消标志（由命令层持有，点击取消即置位）。
    /// - `progress`: 进度回调 `(已复制字节, 总字节)`；首尾各必发一次。
    ///
    /// # Returns
    /// true = 已切换为内存后备；false = 用户取消（后备保持不变）。
    ///
    /// # Errors
    /// 索引未完成或内存分配失败时返回错误；取消不算错误。
    pub fn load_into_ram(
        &mut self,
        cancel: &AtomicBool,
        progress: &mut impl FnMut(u64, u64),
    ) -> Result<bool, String> {
        let total = self.bytes().len();
        if self.is_ram_backed() {
            progress(total as u64, total as u64);
            return Ok(true);
        }
        // 后台索引恰好完成时先并回正式结构，再判断是否还在索引。
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("索引尚未完成，暂时不能载入内存".into());
        }
        // 先一次性预留，内存不足时在开始拷贝前就失败（而不是拷到一半才 OOM）。
        let mut buf: Vec<u8> = Vec::new();
        buf.try_reserve_exact(total)
            .map_err(|_| format!("内存不足：无法分配 {:.1} MB 文件缓冲", total as f64 / 1048576.0))?;
        const COPY_CHUNK: usize = 8 * 1024 * 1024;
        let mut done = 0usize;
        let mut last_emit: Option<Instant> = None;
        progress(0, total as u64);
        while done < total {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false); // buf 在此丢弃，mmap 仍是后备
            }
            let end = (done + COPY_CHUNK).min(total);
            buf.extend_from_slice(&self.bytes()[done..end]);
            done = end;
            if progress_gate(&mut last_emit, PROGRESS_MIN_GAP) || done == total {
                progress(done as u64, total as u64);
            }
        }
        // 索引已完成且后台线程已退出，切换后旧映射可立即释放。
        self.backing = Backing::Ram(buf);
        progress(total as u64, total as u64);
        Ok(true)
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
                    .saturating_sub(self.record_offset() as usize) as u64,
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
            headerless: self.headerless,
            encoding: match self.enc {
                Encoding::Utf8 => "utf-8".into(),
                Encoding::Gbk => "gbk".into(),
            },
            pending_cols: !self.pending_cols.is_empty(),
            can_undo: self.history_stats().undo_count > 0,
            can_redo: self.history_stats().redo_count > 0,
            indexing,
            ram_backed: self.is_ram_backed(),
            has_path: self.has_path(),
            sorted: self.is_sorted(),
            modified: self.modified(),
            indexed_bytes,
            total_bytes: self.total_bytes,
            open_ms: self.open_ms,
            index_ms,
            index_sync: self.index_sync,
            elapsed_ms,
        }
    }

    /// 外部改动探测（窗口聚焦时前端轮询）。
    pub fn stale(&self) -> bool {
        // 无路径内存档没有磁盘原文件，不存在外部改动。
        if !self.has_path() {
            return false;
        }
        let Ok(m) = fs::metadata(&self.path) else { return true };
        m.len() != self.base_size || m.modified().ok() != Some(self.base_mtime)
    }

    // ---- 列视图（pending 映射） ----

    /// 当前用于读取的结构：pending 存在时只改变投影，不改 committed Schema。
    fn view_schema(&self) -> &Schema {
        self.pending_schema.as_deref().unwrap_or(self.schema.as_ref())
    }

    /// 把当前视图列解析为稳定身份和物理存储来源。
    fn view_column(&self, view_col: u32) -> Option<(ColumnId, ColumnStorage)> {
        let schema = self.view_schema();
        let id = schema.view_id(view_col)?;
        let storage = schema.storage(id)?;
        Some((id, storage))
    }

    /// 写路径只接受 committed 视图列；pending 期间由编辑闸门先行拒绝。
    fn committed_column(&self, view_col: u32) -> Result<(ColumnId, ColumnStorage), String> {
        let id = self.schema.view_id(view_col).ok_or("列号越界")?;
        let storage = self.schema.storage(id).ok_or("列身份不存在")?;
        Ok((id, storage))
    }

    /// 插入行的物理槽：原始列沿用源槽，新增列使用源区之后的稀疏扩展槽。
    fn inserted_slot(&self, storage: ColumnStorage) -> Option<usize> {
        match storage {
            ColumnStorage::Source(slot) => Some(slot as usize),
            ColumnStorage::Inserted(slot) => self.base_ncols.checked_add(slot as usize),
        }
    }

    /// 把 committed 逻辑行转换为插入块物理行；尾部空值不 materialize 存储槽。
    fn logical_rows_to_storage(&self, rows: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, String> {
        rows.into_iter()
            .map(|row| {
                let mut stored = Vec::new();
                for (id, value) in self.schema.visible().iter().zip(row) {
                    let storage = self.schema.storage(*id).ok_or("列身份不存在")?;
                    let slot = self.inserted_slot(storage).ok_or("插入列存储槽越界")?;
                    if slot >= stored.len() {
                        if value.is_empty() {
                            continue;
                        }
                        let end = slot.checked_add(1).ok_or("插入列存储槽越界")?;
                        stored.resize(end, String::new());
                    }
                    stored[slot] = value;
                }
                Ok(stored)
            })
            .collect()
    }

    /// 在列级候选上替换可见列名，保留稳定身份与存储来源。
    fn schema_with_headers(&self, headers: &[String]) -> Result<Schema, String> {
        let mut candidate = (*self.schema).clone();
        let ids = candidate.visible().to_vec();
        if ids.len() != headers.len() {
            return Err("列名与列结构不一致".into());
        }
        for (id, header) in ids.into_iter().zip(headers.iter().cloned()) {
            candidate.rename(id, header)?;
        }
        Ok(candidate)
    }

    /// 映射后列数 = 基准列 + 插入 − 删除。
    fn view_ncols(&self) -> u32 {
        self.view_schema().visible().len() as u32
    }

    /// 映射后表头。无列名模式下列名是虚拟的 列1..N。
    fn view_headers(&self) -> Vec<String> {
        if self.headerless {
            return generic_headers(self.view_ncols() as usize);
        }
        let schema = self.view_schema();
        schema
            .visible()
            .iter()
            .map(|id| schema.resolve(*id).map(|column| column.header.clone()).unwrap_or_default())
            .collect()
    }

    /// committed 逻辑字段数组 → pending 视图字段数组。
    fn map_fields_to_view(&self, fields: &[String]) -> Vec<String> {
        let Some(pending) = &self.pending_schema else {
            return fields.to_vec();
        };
        let positions: HashMap<ColumnId, usize> = self
            .schema
            .visible()
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        pending
            .visible()
            .iter()
            .map(|id| {
                positions
                    .get(id)
                    .and_then(|index| fields.get(*index))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }

    fn require_editable(&self) -> Result<(), String> {
        if !self.pending_cols.is_empty() {
            return Err("列操作待执行：请先执行或撤销列操作".into());
        }
        Ok(())
    }

    // ---- 读路径 ----

    /// 读取源记录字段（解码 + 解析 + pad/truncate + 覆盖层）。
    /// 把物理记录区间 [first, end) 的原始字节整段写入（透传不重编码）。
    /// 端点取记录首行偏移；区间末尾即文件末时用 mmap 长度。
    /// 仅在 save() 透传路径使用：调用方保证行未删除（段已剔除被删行）、无覆盖层。
    fn write_raw_range(&self, w: &mut impl Write, first: u32, end: u32) -> Result<(), String> {
        let mut s = self.idx.record_span(self.bytes().len(), first as usize).0;
        let e = if (end as usize) < self.idx.n_records() {
            self.idx.record_span(self.bytes().len(), end as usize).0
        } else {
            self.bytes().len()
        };
        // BOM 已单独写在文件开头：仅当记录 0 正好从文件字节 0 起才跳过，
        // 有前导空行时记录 0 起点在 BOM 之后，不能盲跳。
        if first == 0 && self.has_bom && s == 0 {
            s = 3;
        }
        w.write_all(&self.bytes()[s..e]).map_err(|e| e.to_string())
    }

    /// 物理记录 rec 的可见字节范围；记录 0 统一剥掉文件头 BOM。
    fn src_span(&self, rec: u32) -> Option<(usize, usize)> {
        let (mut s, e) = self.record_span_visible(rec as usize)?;
        if rec == 0 && self.has_bom && s == 0 {
            s = 3;
        }
        Some((s, e))
    }

    /// 读取原始记录物理字段；覆盖层不在这里按键应用。
    fn raw_src_fields(&self, rec: u32) -> Vec<String> {
        let Some((s, e)) = self.src_span(rec) else {
            return vec![String::new(); self.base_ncols];
        };
        let mut fields = parse_record(&decode_slice(self.enc, &self.bytes()[s..e]), b',');
        if fields.len() < self.base_ncols {
            fields.resize(self.base_ncols, String::new());
        } else if fields.len() > self.base_ncols {
            fields.truncate(self.base_ncols);
        }
        fields
    }

    /// 按 committed Schema 读取源记录；Source 读物理槽，Inserted 只读覆盖层。
    fn src_fields(&self, rec: u32) -> Vec<String> {
        let raw = self.raw_src_fields(rec);
        self.schema
            .visible()
            .iter()
            .map(|id| {
                let mut value = match self.schema.storage(*id) {
                    Some(ColumnStorage::Source(slot)) => {
                        raw.get(slot as usize).cloned().unwrap_or_default()
                    }
                    _ => String::new(),
                };
                if let Some(edited) = self.overlay.get(&(rec, *id)) {
                    value.clone_from(edited);
                }
                value
            })
            .collect()
    }

    /// 按 committed Schema 读取插入行；扩展槽缺值保持稀疏，不逐行补齐。
    fn ins_fields(&self, block: u32, row: u32) -> Vec<String> {
        let cells = self
            .seg
            .blocks
            .get(block as usize)
            .and_then(|block| block.as_ref())
            .and_then(|rows| rows.get(row as usize));
        self.schema
            .visible()
            .iter()
            .map(|id| {
                self.schema
                    .storage(*id)
                    .and_then(|storage| self.inserted_slot(storage))
                    .and_then(|slot| cells.and_then(|cells| cells.get(slot)))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }

    /// 取 vrow 的视图行（排序映射 + pending 列映射；渐进期按模式偏移换算）。
    fn row_at(&self, vrow: u64) -> Option<Vec<String>> {
        let vrow = self.orig_vrow(vrow);
        if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return None;
            }
            let rec = vrow as u32 + self.record_offset();
            return Some(self.map_fields_to_view(&self.src_fields(rec)));
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

    /// 显示行 vrow 的视图列 col 是否非空（含覆盖层/插入行/pending 列映射）。
    ///
    /// 只取目标列，不构造整行字段数组；大文件边缘查找逐行调用。
    ///
    /// # Parameters
    /// - `vrow`: 显示行号（筛选视图调用方已换算成命中行的显示位置）。
    /// - `col`: 视图列号。
    ///
    /// # Returns
    /// 单元格有内容时为 true；越界/虚拟待插入列/未索引行返回 false。
    fn cell_nonempty_at(&self, vrow: u64, col: u32) -> bool {
        let vrow = self.orig_vrow(vrow);
        let Some((id, storage)) = self.view_column(col) else {
            return false;
        };
        let resolved = if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return false;
            }
            ResolvedRow::Src(vrow as u32 + self.record_offset())
        } else {
            match self.seg.resolve(vrow) {
                Some(row) => row,
                None => return false,
            }
        };
        match resolved {
            ResolvedRow::Src(rec) => {
                if let Some(value) = self.overlay.get(&(rec, id)) {
                    return !value.is_empty();
                }
                match storage {
                    ColumnStorage::Source(slot) => self.record_field_nonempty(rec, slot),
                    ColumnStorage::Inserted(_) => false,
                }
            }
            ResolvedRow::Ins { block, row } => self
                .inserted_slot(storage)
                .and_then(|slot| {
                    self.seg
                        .blocks
                        .get(block as usize)
                        .and_then(|block| block.as_ref())
                        .and_then(|rows| rows.get(row as usize))
                        .and_then(|cells| cells.get(slot))
                })
                .is_some_and(|value| !value.is_empty()),
        }
    }

    /// 物理记录 rec 的目标列是否非空（不分配整行字段）。
    ///
    /// UTF-8 直接在字节上按结构字符扫描（ASCII 分隔符不会出现在多字节序列
    /// 内部）；GBK 必须先解码，避免 trail byte 被误认成逗号/引号。
    ///
    /// # Parameters
    /// - `rec`: 物理记录号。
    /// - `col`: 物理列号。
    ///
    /// # Returns
    /// 字段有内容时为 true。
    fn record_field_nonempty(&self, rec: u32, col: u32) -> bool {
        let Some((s, e)) = self.src_span(rec) else {
            return false;
        };
        let bytes = &self.bytes()[s..e];
        if self.enc == Encoding::Gbk {
            let text = decode_slice(self.enc, bytes);
            field_nonempty(text.as_bytes(), b',', col as usize)
        } else {
            field_nonempty(bytes, b',', col as usize)
        }
    }

    /// Excel 风格纵向数据边缘查找（Ctrl+↑/↓）。
    ///
    /// 语义：
    /// - 当前格非空：跳到当前连续非空块的上/下边缘；
    /// - 已在边缘：越过空档，跳到下一条非空块的边缘格；
    /// - 当前格为空：向该方向找下一个非空格；
    /// - 该方向无数据：落到可导航行范围的首/末行。
    ///
    /// 筛选视图按命中行序号导航；排序视图按当前显示顺序导航。
    ///
    /// # Parameters
    /// - `from`: 当前视图行号（筛选视图为命中序号）。
    /// - `col`: 视图列号。
    /// - `down`: true=向下；false=向上。
    /// - `filtered`: 是否按当前筛选命中集导航。
    ///
    /// # Returns
    /// 目标视图行号；空表/无命中时为 None。
    ///
    /// # Errors
    /// `col` 超出当前视图列数时返回错误字符串。
    pub fn find_vertical_edge(
        &self,
        from: u64,
        col: u32,
        down: bool,
        filtered: bool,
    ) -> Result<Option<u64>, String> {
        if col >= self.view_ncols() {
            return Err("列号越界".into());
        }
        if filtered {
            let Some(hits) = &self.filter else {
                return Ok(None);
            };
            return Ok(find_vertical_edge_by(
                from,
                hits.len() as u64,
                down,
                |i| self.cell_nonempty_at(hits[i as usize] as u64, col),
            ));
        }
        let total = if self.partial.is_some() {
            self.indexed_data_rows()
        } else {
            self.seg.vrow_count()
        };
        Ok(find_vertical_edge_by(from, total, down, |vrow| {
            self.cell_nonempty_at(vrow, col)
        }))
    }

    // ---- 编辑 ----

    /// 读取单元格当前值（编辑前旧值用）。
    fn cell_value(&self, cell: CellId) -> String {
        match cell.row {
            RowId::Source(rec) => {
                let col = cell.col;
                if let Some(value) = self.overlay.get(&(rec, col)) {
                    return value.clone();
                }
                match self.schema.storage(col) {
                    Some(ColumnStorage::Source(slot)) => self
                        .raw_src_fields(rec)
                        .get(slot as usize)
                        .cloned()
                        .unwrap_or_default(),
                    _ => String::new(),
                }
            }
            RowId::Inserted { block, row } => {
                let col = cell.col;
                let Some(slot) = self
                    .schema
                    .storage(col)
                    .and_then(|storage| self.inserted_slot(storage))
                else {
                    return String::new();
                };
                self.seg
                    .blocks
                    .get(block as usize)
                    .and_then(|block| block.as_ref())
                    .and_then(|rows| rows.get(row as usize))
                    .and_then(|cells| cells.get(slot))
                    .cloned()
                    .unwrap_or_default()
            }
        }
    }

    /// 将批前文本转换为稳定历史值；未覆盖源格只记录 backing 范围。
    fn history_value(&self, cell: CellId, old: String) -> ValueRef {
        if !old.is_empty() {
            if let RowId::Source(rec) = cell.row {
                if !self.overlay.contains_key(&(rec, cell.col))
                    && matches!(self.schema.storage(cell.col), Some(ColumnStorage::Source(_)))
                {
                    if let Some((start, end)) = self.src_span(rec) {
                        return ValueRef::Source {
                            row: cell.row,
                            col: cell.col,
                            offset: start as u64,
                            raw_len: (end - start) as u64,
                            utf8_len: old.len() as u64,
                            index_epoch: self.index_epoch,
                        };
                    }
                }
            }
        }
        ValueRef::text(old)
    }

    /// 将显示行与 committed 视图列解析为稳定格地址，供批量写入预检使用。
    ///
    /// # Parameters
    /// - `vrow`: 当前显示行号。
    /// - `col`: committed 视图列号。
    ///
    /// # Returns
    /// 稳定的源记录或插入块单元格地址。
    ///
    /// # Errors
    /// 行号、列号越界时返回中文错误；不会修改数据。
    fn resolve_batch_cell(&self, vrow: u64, col: u32) -> Result<CellId, String> {
        let (col, _) = self.committed_column(col)?;
        let vrow = self.orig_vrow(vrow);
        let resolved = if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return Err("行号越界".into());
            }
            let rec = u32::try_from(vrow)
                .ok()
                .and_then(|row| row.checked_add(self.record_offset()))
                .ok_or("行号越界")?;
            ResolvedRow::Src(rec)
        } else {
            self.seg.resolve(vrow).ok_or("行号越界")?
        };
        Ok(match resolved {
            ResolvedRow::Src(rec) => CellId { row: RowId::Source(rec), col },
            ResolvedRow::Ins { block, row } => CellId { row: RowId::Inserted { block, row }, col },
        })
    }

    /// 完整预检并生成稀疏格快照：重复目标后者胜，最终同值项剔除。
    ///
    /// # Parameters
    /// - `cells`: `(显示行, 列, 新值)` 输入序列。
    ///
    /// # Returns
    /// 仅含实际变化项的快照；顺序稳定，旧值只读取批前状态一次。
    ///
    /// # Errors
    /// 任一目标非法时返回错误，且不写任何单元格。
    fn prepare_cell_changes(
        &self,
        cells: Vec<(u64, u32, String)>,
    ) -> Result<Vec<CellChange>, String> {
        // 先解析全部地址，保证后置非法项不会留下前置半次写入。
        let mut resolved = Vec::with_capacity(cells.len());
        for (vrow, col, value) in cells {
            resolved.push((self.resolve_batch_cell(vrow, col)?, value));
        }
        let mut latest: HashMap<CellId, usize> = HashMap::with_capacity(resolved.len());
        let mut deduped: Vec<(CellId, String)> = Vec::with_capacity(resolved.len());
        for (cell, value) in resolved {
            if let Some(index) = latest.get(&cell).copied() {
                deduped[index].1 = value; // 同一稳定格后者胜，旧值仍取批前一次
            } else {
                latest.insert(cell, deduped.len());
                deduped.push((cell, value));
            }
        }
        let mut changes = Vec::with_capacity(deduped.len());
        for (cell, new) in deduped {
            let old = self.cell_value(cell);
            if old != new {
                let old = self.history_value(cell, old);
                changes.push(CellChange { cell, old, new: ValueRef::text(new) });
            }
        }
        Ok(changes)
    }

    pub fn modified(&self) -> bool {
        self.savepoint_detached || self.revision != 0
    }

    pub fn history_stats(&self) -> HistoryStats {
        self.history.stats()
    }

    pub fn set_history_budget(&mut self, budget_bytes: u64) -> Result<BudgetChange, String> {
        let next_generation = self.next_history_generation()?;
        let change = self.history.set_budget(budget_bytes);
        if change.fixed_point {
            self.fixed_point = true;
            if self.revision != 0 {
                self.savepoint_detached = true;
            }
        }
        self.history_generation = next_generation;
        Ok(change)
    }

    fn next_history_generation(&self) -> Result<u64, String> {
        self.history_generation.checked_add(1).ok_or("历史代际溢出".into())
    }

    fn save_fixed_point_request(
        &self,
        kind: FixedPointKind,
        operation: String,
    ) -> FixedPointRequest {
        let stats = self.history.stats();
        FixedPointRequest {
            kind,
            operation,
            required_bytes: stats.bytes,
            budget_bytes: stats.budget_bytes,
            preflight_id: self.history_generation,
        }
    }

    fn clear_history_at_fixed_point(&mut self, next_generation: u64) {
        self.history.clear();
        self.fixed_point = true;
        if self.revision != 0 {
            self.savepoint_detached = true;
        }
        self.history_generation = next_generation;
    }

    fn preflight_history(
        &self,
        op: Op,
        operation: &str,
        approval: Approval,
    ) -> Result<HistoryDecision, String> {
        let entry = op.history_entry().map_err(history_cost_error)?;
        self.preflight_history_entry(entry, operation, approval)
    }

    fn preflight_history_entry(
        &self,
        entry: HistoryEntry<Op>,
        operation: &str,
        approval: Approval,
    ) -> Result<HistoryDecision, String> {
        // 所有显式批准都必须对应当前代际，不能因重试步骤变小而绕过。
        if approval.allow_fixed_point
            && approval.preflight_id != Some(self.history_generation)
        {
            return Err("固定点预检已过期，请重试".into());
        }
        // 两个计数器都必须在任何数据写入前验证，finish_history 会再次取值提交。
        self.revision
            .checked_add(entry.revision_delta)
            .ok_or("修订号溢出")?;
        self.next_history_generation()?;
        let budget_bytes = self.history.stats().budget_bytes;
        let prepared = self.history.prepare(entry).map_err(history_cost_error)?;
        if prepared.requires_fixed_point() {
            if approval.allow_fixed_point {
                return Ok(HistoryDecision::FixedPoint(prepared));
            }
            let mut outcome = EditOutcome::unchanged();
            outcome.fixed_point = Some(FixedPointRequest {
                kind: FixedPointKind::StepOverBudget,
                operation: operation.into(),
                required_bytes: prepared.entry_bytes(),
                budget_bytes,
                preflight_id: self.history_generation,
            });
            return Ok(HistoryDecision::Confirm(outcome));
        }
        Ok(HistoryDecision::Normal(prepared))
    }

    fn finish_history(
        &mut self,
        prepared: Prepared<Op>,
        fixed_point: bool,
        inserted_rows: u64,
        removed_rows: u64,
    ) -> Result<EditOutcome, String> {
        let next_revision = self
            .revision
            .checked_add(prepared.entry().revision_delta)
            .ok_or("修订号溢出")?;
        let next_generation = self.next_history_generation()?;
        if fixed_point {
            self.clear_history_at_fixed_point(next_generation);
        } else {
            // 在 revision 更新前标记：redo 中的保存点即将被新分支清除。
            if self.revision < 0 {
                self.savepoint_detached = true;
            }
            self.history.commit_prepared(prepared);
            self.history_generation = next_generation;
        }
        self.revision = next_revision;
        Ok(EditOutcome {
            changed: true,
            inserted_rows,
            removed_rows,
            fixed_point: None,
            cancelled: false,
        })
    }

    /// 按结构化目标清除内容；普通操作保留稀疏旧值，超预算确认后流式建立固定点。
    pub fn clear(
        &mut self,
        target: ClearTarget,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        self.refresh_indexed();
        let total_rows = self.indexed_data_rows();
        let ncols = self.schema.visible().len();
        let target = target.normalize(total_rows, ncols)?;

        // 第一遍只累计实际非空旧值的确定性成本，不创建全量 CellChange。
        let mut estimated = Cost::new();
        estimated.add_value::<Op>().map_err(history_cost_error)?;
        let mut change_count = 0u64;
        target.visit(total_rows, ncols, |vrow, col| {
            let cell = self.resolve_batch_cell(vrow, col)?;
            let old = self.cell_value(cell);
            if old.is_empty() {
                return Ok(());
            }
            change_count = change_count
                .checked_add(1)
                .ok_or_else(|| history_cost_error(HistoryError::CostOverflow))?;
            estimated.add_value::<CellChange>().map_err(history_cost_error)?;
            let old_bytes = history_text_bytes(old.len() as u64).map_err(history_cost_error)?;
            estimated.add(old_bytes).map_err(history_cost_error)?;
            // 新空值共享一份空文本；实际共享账目在 preflight_history 复核。
            Ok(())
        })?;
        if change_count == 0 {
            return Ok(EditOutcome::unchanged());
        }
        let estimated_bytes = estimated.bytes();
        let budget_bytes = self.history.stats().budget_bytes;
        if estimated_bytes > budget_bytes {
            // 用估算成本做固定点预检；确认后无需保存任何旧值。
            let entry = HistoryEntry::new(
                Op::SetCells { changes: Vec::new() },
                estimated_bytes,
                Vec::new(),
                1,
            );
            return match self.preflight_history_entry(entry, "清除区域", approval)? {
                HistoryDecision::Confirm(outcome) => Ok(outcome),
                HistoryDecision::FixedPoint(prepared) => {
                    self.stream_clear_target(&target, total_rows)?;
                    self.last_spec = None;
                    self.finish_history(prepared, true, 0, 0)
                }
                HistoryDecision::Normal(_) => Err("清除固定点预检状态错误".into()),
            };
        }

        // 估算在预算内时，第二遍按变化数预分配，再用实际 Vec/String 容量复核。
        let capacity = usize::try_from(change_count)
            .map_err(|_| history_cost_error(HistoryError::CostOverflow))?;
        let mut changes = Vec::with_capacity(capacity);
        target.visit(total_rows, ncols, |vrow, col| {
            let cell = self.resolve_batch_cell(vrow, col)?;
            let old = self.cell_value(cell);
            if !old.is_empty() {
                let old = self.history_value(cell, old);
                changes.push(CellChange { cell, old, new: ValueRef::text(String::new()) });
            }
            Ok(())
        })?;
        debug_assert_eq!(changes.len(), capacity);
        let (mut prepared, fixed_point) = match self.preflight_history(
            Op::SetCells { changes },
            "清除区域",
            approval,
        )? {
            HistoryDecision::Normal(prepared) => (prepared, false),
            HistoryDecision::FixedPoint(prepared) => (prepared, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        self.apply(&mut prepared.entry_mut().op);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, 0, 0)
    }

    /// 固定点确认后流式清除非空旧值，不保留全量历史快照。
    fn stream_clear_target(
        &mut self,
        target: &NormalizedClear,
        total_rows: u64,
    ) -> Result<(), String> {
        let ncols = self.schema.visible().len();
        target.visit(total_rows, ncols, |vrow, col| {
            let cell = self.resolve_batch_cell(vrow, col)?;
            if !self.cell_value(cell).is_empty() {
                self.write_cell(cell, "");
            }
            Ok(())
        })
    }

    /// 将一组单元格修改作为一个用户操作写入。
    pub fn set_cells(
        &mut self,
        cells: Vec<(u64, u32, String)>,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        self.refresh_indexed();
        let changes = self.prepare_cell_changes(cells)?;
        if changes.is_empty() {
            return Ok(EditOutcome::unchanged());
        }
        let (mut prepared, fixed_point) = match self.preflight_history(
            Op::SetCells { changes },
            "批量编辑单元格",
            approval,
        )? {
            HistoryDecision::Normal(p) => (p, false),
            HistoryDecision::FixedPoint(p) => (p, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        self.apply(&mut prepared.entry_mut().op);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, 0, 0)
    }

    /// 粘贴不规则行块；既有格与末尾扩行只占一步历史。
    pub fn paste_rows(
        &mut self,
        vrow: u64,
        col: u32,
        mut rows: Vec<Vec<String>>,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        self.refresh_indexed();
        if rows.is_empty() {
            return Ok(EditOutcome::unchanged());
        }
        let max_width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let ncols = self.schema.visible().len();
        let mut paste_start = 0;
        if max_width > 0 {
            let start_id = self.schema.view_id(col).ok_or("粘贴列号越界")?;
            paste_start = self
                .schema
                .visible()
                .iter()
                .position(|id| *id == start_id)
                .ok_or("粘贴列号越界")?;
            let col_end = paste_start.checked_add(max_width).ok_or("列号越界")?;
            if col_end > ncols {
                return Err("粘贴列号越界".into());
            }
        }
        let input_count = u64::try_from(rows.len()).map_err(|_| "粘贴行数过多")?;
        let total = if self.partial.is_some() {
            self.indexed_data_rows()
        } else {
            self.seg.vrow_count()
        };
        if vrow > total {
            return Err("粘贴位置越界".into());
        }
        let end = vrow.checked_add(input_count).ok_or("粘贴位置越界")?;
        let inserted_rows = end.saturating_sub(total);
        if inserted_rows > 0 {
            if self.order.is_some() {
                return Err("排序模式下暂不支持扩行粘贴，请先退出排序".into());
            }
            if self.is_indexing() {
                return Err("文件仍在索引中，暂不支持扩行粘贴".into());
            }
        }
        let existing_count = input_count.min(total - vrow) as usize;
        let mut cells = Vec::new();
        for (i, row) in rows[..existing_count].iter().enumerate() {
            for (j, value) in row.iter().enumerate() {
                cells.push((vrow + i as u64, col + j as u32, value.clone()));
            }
        }
        let inserted = if inserted_rows > 0 {
            rows.split_off(existing_count)
        } else {
            Vec::new()
        };
        let inserted = inserted
            .into_iter()
            .map(|row| {
                let mut logical = vec![String::new(); ncols];
                for (j, value) in row.into_iter().enumerate() {
                    logical[paste_start + j] = value;
                }
                logical
            })
            .collect();
        let inserted = self.logical_rows_to_storage(inserted)?;
        let changes = self.prepare_cell_changes(cells)?;
        if changes.is_empty() && inserted.is_empty() {
            return Ok(EditOutcome::unchanged());
        }
        let inserted = if inserted.is_empty() {
            None
        } else {
            Some(PasteInsert { vpos: total, block: 0, rows: inserted })
        };
        let (mut prepared, fixed_point) = match self.preflight_history(
            Op::Paste { changes, inserted },
            "粘贴区域",
            approval,
        )? {
            HistoryDecision::Normal(p) => (p, false),
            HistoryDecision::FixedPoint(p) => (p, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        self.apply(&mut prepared.entry_mut().op);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, inserted_rows, 0)
    }

    /// 写单元格；单步超预算时先返回固定点请求，不写数据。
    pub fn set_cell(
        &mut self,
        vrow: u64,
        col: u32,
        value: String,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        let col = self.schema.view_id(col).ok_or("列号越界")?;
        let vrow = self.orig_vrow(vrow);
        let resolved = if self.partial.is_some() {
            if vrow >= self.indexed_data_rows() {
                return Err("行号越界".into());
            }
            ResolvedRow::Src(vrow as u32 + self.record_offset())
        } else {
            self.seg.resolve(vrow).ok_or("行号越界")?
        };
        let cell = match resolved {
            ResolvedRow::Src(rec) => CellId { row: RowId::Source(rec), col },
            ResolvedRow::Ins { block, row } => CellId { row: RowId::Inserted { block, row }, col },
        };
        let old = self.cell_value(cell);
        if old == value {
            return Ok(EditOutcome::unchanged());
        }
        let (mut prepared, fixed_point) = match self.preflight_history(
            Op::SetCell { cell, old: self.history_value(cell, old), new: ValueRef::text(value) },
            "编辑单元格",
            approval,
        )? {
            HistoryDecision::Normal(p) => (p, false),
            HistoryDecision::FixedPoint(p) => (p, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        self.apply(&mut prepared.entry_mut().op);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, 0, 0)
    }

    /// 在 vpos 处插入行（rows 为 None 时插 count 行空行）。
    pub fn insert_rows(
        &mut self,
        vpos: u64,
        rows: Option<Vec<Vec<String>>>,
        count: u64,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        if self.order.is_some() {
            return Err("排序模式下暂不支持插入/删除行，请先退出排序".into());
        }
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持插入/删除行".into());
        }
        if vpos > self.seg.vrow_count() {
            return Err("插入位置越界".into());
        }
        let rows = match rows {
            Some(rows) => rows,
            None => {
                let count = usize::try_from(count).map_err(|_| "插入行数过多")?;
                vec![vec![String::new(); self.schema.visible().len()]; count]
            }
        };
        if rows.is_empty() {
            return Ok(EditOutcome::unchanged());
        }
        let ncols = self.schema.visible().len();
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|mut row| {
                if row.len() < ncols {
                    row.resize(ncols, String::new());
                } else if row.len() > ncols {
                    row.truncate(ncols);
                }
                row
            })
            .collect();
        let rows = self.logical_rows_to_storage(rows)?;
        let inserted_rows = rows.len() as u64;
        let (mut prepared, fixed_point) = match self.preflight_history(
            Op::InsertRows { vpos, block: 0, rows },
            "插入行",
            approval,
        )? {
            HistoryDecision::Normal(p) => (p, false),
            HistoryDecision::FixedPoint(p) => (p, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        self.apply(&mut prepared.entry_mut().op);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, inserted_rows, 0)
    }

    /// 借用当前插入行计算完整删除成本，避免为预检克隆行内容。
    /// 删除前一次性保存全部行内容；条目只引用共享快照及行号。
    fn remove_rows_history_entry(&self, vrows: &[u64]) -> Result<HistoryEntry<Op>, String> {
        let mut resolved = Vec::with_capacity(vrows.len());
        let mut rows = Vec::with_capacity(vrows.len());
        for &vpos in vrows {
            let identity = self.seg.resolve(vpos).ok_or("行号越界")?;
            let cells = match identity {
                ResolvedRow::Src(rec) => self.src_fields(rec),
                ResolvedRow::Ins { block, row } => self.seg.blocks
                    .get(block as usize)
                    .and_then(Option::as_ref)
                    .and_then(|rows| rows.get(row as usize))
                    .cloned()
                    .ok_or("行号越界")?,
            };
            resolved.push((vpos, identity));
            rows.push(cells);
        }
        let snapshot = RowSnapshot::write(&self.spool, rows.into_iter())?;
        let items = resolved.into_iter().enumerate().map(|(index, (vpos, identity))| {
            let snapshot_row = u32::try_from(index).map_err(|_| "删除快照行数超过 u32")?;
            let snapshot = snapshot.clone();
            Ok(match identity {
                ResolvedRow::Src(rec) => DeletedRow::Src { vpos, rec, snapshot, snapshot_row },
                ResolvedRow::Ins { block, row } => {
                    DeletedRow::Ins { vpos, block, row, snapshot, snapshot_row }
                }
            })
        }).collect::<Result<Vec<_>, String>>()?;
        Op::RemoveRows { items }.history_entry().map_err(history_cost_error)
    }

    /// 删除一组 vrow；确认后才执行并收集真实删除快照。
    pub fn remove_rows(
        &mut self,
        mut vrows: Vec<u64>,
        approval: Approval,
    ) -> Result<EditOutcome, String> {
        self.require_editable()?;
        if self.order.is_some() {
            return Err("排序模式下暂不支持插入/删除行，请先退出排序".into());
        }
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
        if self.headerless && !vrows.is_empty() && vrows.len() as u64 >= total {
            return Err("无列名模式需至少保留一行".into());
        }
        if vrows.is_empty() {
            return Ok(EditOutcome::unchanged());
        }
        let entry = self.remove_rows_history_entry(&vrows)?;
        let (prepared, fixed_point) = match self.preflight_history_entry(
            entry,
            "删除行",
            approval,
        )? {
            HistoryDecision::Normal(p) => (p, false),
            HistoryDecision::FixedPoint(p) => (p, true),
            HistoryDecision::Confirm(outcome) => return Ok(outcome),
        };
        let (removed_rows, complete, rollbacks) = {
            let mut rollbacks = Vec::with_capacity(vrows.len());
            let mut complete = true;
            for &vpos in vrows.iter().rev() {
                let Some((index, off)) = self.seg.locate(vpos) else {
                    complete = false;
                    break;
                };
                let original_seg = self.seg.segs[index];
                let removed_seg = match original_seg.kind {
                    SegKind::Ins { block } => self.seg.blocks
                        .get(block as usize)
                        .and_then(Option::as_ref)
                        .is_some_and(|rows| rows.len() == 1),
                    SegKind::Src { .. } => false,
                };
                let tombstone_was_present = match original_seg.kind {
                    SegKind::Src { start, .. } => start
                        .checked_add(off)
                        .is_some_and(|rec| self.seg.tombstones.contains(&rec)),
                    SegKind::Ins { .. } => false,
                };
                let Some(cells) = self.seg.delete_one(vpos) else {
                    complete = false;
                    break;
                };
                rollbacks.push(DeleteRollback {
                    index,
                    off,
                    seg: original_seg,
                    removed_seg,
                    tombstone_was_present,
                    cells,
                });
            }
            (vrows.len() as u64, complete, rollbacks)
        };
        if !complete {
            self.seg.rollback_deleted(rollbacks);
            return Err("内部行索引不一致，删除已取消".into());
        }
        drop(rollbacks);
        self.last_spec = None;
        self.finish_history(prepared, fixed_point, 0, removed_rows)
    }

    /// 原地执行并归一化插入块号；不克隆整条操作。
    fn apply(&mut self, op: &mut Op) {
        match op {
            Op::SetCell { cell, new, .. } => self.write_value(*cell, new),
            Op::SetCells { changes } => {
                for change in changes {
                    self.write_value(change.cell, &change.new);
                }
            }
            Op::Paste { changes, inserted } => {
                if let Some(inserted) = inserted {
                    inserted.block = self.seg.insert_block(inserted.vpos, inserted.rows.clone());
                }
                for change in changes {
                    self.write_value(change.cell, &change.new);
                }
            }
            Op::InsertRows { vpos, block, rows } => {
                *block = self.seg.insert_block(*vpos, rows.clone());
            }
            Op::RemoveRows { items } => {
                items.sort_by_key(|item| match item {
                    DeletedRow::Src { vpos, .. }
                    | DeletedRow::Ins { vpos, .. }
                    | DeletedRow::Suspended { vpos, .. } => *vpos,
                });
                for item in items.iter().rev() {
                    let vpos = match item {
                        DeletedRow::Src { vpos, .. }
                    | DeletedRow::Ins { vpos, .. }
                    | DeletedRow::Suspended { vpos, .. } => *vpos,
                    };
                    let _ = self.seg.delete_one(vpos);
                }
            }
        }
    }

    /// 从共享文本或其原始 backing 记录读取历史值，再写入稳定格地址。
    fn write_value(&mut self, cell: CellId, value: &ValueRef) {
        match value {
            ValueRef::Text(text) => self.write_cell(cell, &text.text),
            ValueRef::Source { .. } => {
                if let Ok(value) = self.source_value_text(value) {
                    self.write_cell(cell, &value);
                }
            }
        }
    }

    fn write_cell(&mut self, cell: CellId, value: &str) {
        match cell.row {
            RowId::Source(rec) => {
                let col = cell.col;
                self.overlay.insert((rec, col), value.to_string());
            }
            RowId::Inserted { block, row } => {
                let col = cell.col;
                let Some(slot) = self
                    .schema
                    .storage(col)
                    .and_then(|storage| self.inserted_slot(storage))
                else {
                    return;
                };
                if let Some(Some(rows)) = self.seg.blocks.get_mut(block as usize) {
                    if let Some(cells) = rows.get_mut(row as usize) {
                        if slot >= cells.len() {
                            cells.resize(slot + 1, String::new());
                        }
                        cells[slot] = value.to_string();
                    }
                }
            }
        }
    }

    /// 撤销；逆操作失败时把同一条目放回原栈，数据与代际不变。
    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.history.take_undo() else { return false };
        let Some(next_revision) = self.revision.checked_sub(entry.revision_delta) else {
            self.history.restore_undo(entry);
            return false;
        };
        let Some(next_generation) = self.history_generation.checked_add(1) else {
            self.history.restore_undo(entry);
            return false;
        };
        if !self.apply_inverse(&entry.op) {
            self.history.restore_undo(entry);
            return false;
        }
        self.last_spec = None;
        self.revision = next_revision;
        self.history_generation = next_generation;
        self.history.finish_undo(entry);
        true
    }

    /// 重做；移动原条目并只更新真实块号。
    pub fn redo(&mut self) -> bool {
        let Some(mut entry) = self.history.take_redo() else { return false };
        let Some(next_revision) = self.revision.checked_add(entry.revision_delta) else {
            self.history.restore_redo(entry);
            return false;
        };
        let Some(next_generation) = self.history_generation.checked_add(1) else {
            self.history.restore_redo(entry);
            return false;
        };
        let old_block = match &entry.op {
            Op::InsertRows { block, .. } => Some(*block),
            Op::Paste { inserted: Some(inserted), .. } => Some(inserted.block),
            _ => None,
        };
        self.apply(&mut entry.op);
        let new_block = match &entry.op {
            Op::InsertRows { block, .. } => Some(*block),
            Op::Paste { inserted: Some(inserted), .. } => Some(inserted.block),
            _ => None,
        };
        if let (Some(old), Some(new)) = (old_block, new_block) {
            if old != new {
                self.rebind_redo_cells(old, new);
            }
        }
        self.last_spec = None;
        self.revision = next_revision;
        self.history_generation = next_generation;
        self.history.finish_redo(entry);
        true
    }

    /// 插行/扩行粘贴重做换块后，重写后续重做记录里的稳定格地址。
    fn rebind_redo_cells(&mut self, old_block: u32, new_block: u32) {
        fn rebind(cell: &mut CellId, old_block: u32, new_block: u32) {
            if let RowId::Inserted { block, .. } = &mut cell.row {
                if *block == old_block {
                    *block = new_block;
                }
            }
        }
        for op in self.history.redo_ops_mut() {
            match op {
                Op::SetCell { cell, .. } => rebind(cell, old_block, new_block),
                Op::SetCells { changes } | Op::Paste { changes, .. } => {
                    for change in changes {
                        rebind(&mut change.cell, old_block, new_block);
                    }
                }
                Op::InsertRows { .. } => {}
                Op::RemoveRows { items } => {
                    for item in items {
                        if let DeletedRow::Ins { block, .. } = item {
                            if *block == old_block {
                                *block = new_block;
                            }
                        }
                    }
                }
            }
        }
    }

    /// 保存后撤销物化插行：后续 redo 中的 Source 行改回原 Inserted 身份。
    fn rebind_redo_materialized(&mut self, mappings: &[(u32, u32)], block: u32) {
        if mappings.is_empty() {
            return;
        }
        let rows_by_rec: HashMap<u32, u32> = mappings.iter().copied().collect();
        fn rebind_cell(cell: &mut CellId, mappings: &HashMap<u32, u32>, block: u32) {
            if let RowId::Source(rec) = cell.row {
                if let Some(row) = mappings.get(&rec) {
                    cell.row = RowId::Inserted { block, row: *row };
                }
            }
        }
        for op in self.history.redo_ops_mut() {
            match op {
                Op::SetCell { cell, .. } => rebind_cell(cell, &rows_by_rec, block),
                Op::SetCells { changes } | Op::Paste { changes, .. } => {
                    for change in changes {
                        rebind_cell(&mut change.cell, &rows_by_rec, block);
                    }
                }
                Op::RemoveRows { items } => {
                    for item in items {
                        let replacement = match item {
                            DeletedRow::Src { vpos, rec, snapshot, snapshot_row } => {
                                rows_by_rec.get(rec).map(|row| DeletedRow::Ins {
                                    vpos: *vpos,
                                    block,
                                    row: *row,
                                    snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                })
                            }
                            DeletedRow::Ins { .. } | DeletedRow::Suspended { .. } => None,
                        };
                        if let Some(replacement) = replacement {
                            *item = replacement;
                        }
                    }
                }
                Op::InsertRows { .. } => {}
            }
        }
    }

    /// 校验并摘除撤销插入；保存后的物化 Source 行也按同一逻辑块处理。
    fn remove_inserted_block_for_undo(
        &mut self,
        vpos: u64,
        block: u32,
        rows: &[Vec<String>],
    ) -> bool {
        if let Some((i, off)) = self.seg.locate(vpos) {
            if let SegKind::Ins { block: current } = self.seg.segs[i].kind {
                let covers = off == 0 && self.seg.segs[i].len == rows.len() as u32;
                if covers && current != block {
                    return false;
                }
                let same = covers
                    && self.seg.blocks.get(current as usize)
                        .and_then(Option::as_ref)
                        .is_some_and(|current_rows| current_rows.as_slice() == rows);
                if same {
                    self.seg.segs.remove(i);
                    self.seg.blocks[current as usize] = None;
                    return true;
                }
            }
        }

        // 保存把插入行物化为 Source。先验证整段身份和最终内容，再倒序删除，
        // 使任何可预见失败都发生在首次写入之前。
        let mut materialized = Vec::new();
        for (offset, expected) in rows.iter().enumerate() {
            let Ok(offset_u64) = u64::try_from(offset) else { return false };
            let Some(vrow) = vpos.checked_add(offset_u64) else { return false };
            let Some(resolved) = self.seg.resolve(vrow) else { return false };
            let actual = match resolved {
                ResolvedRow::Src(rec) => {
                    if rec.checked_add(1).is_none() {
                        return false;
                    }
                    self.src_fields(rec)
                }
                ResolvedRow::Ins { block, row } => {
                    let exists = self.seg.blocks.get(block as usize)
                        .and_then(Option::as_ref)
                        .and_then(|block_rows| block_rows.get(row as usize))
                        .is_some();
                    if !exists {
                        return false;
                    }
                    self.ins_fields(block, row)
                }
            };
            if actual.as_slice() != expected.as_slice() {
                return false;
            }
            if let ResolvedRow::Src(rec) = resolved {
                let Ok(row) = u32::try_from(offset) else { return false };
                materialized.push((rec, row));
            }
        }
        for offset in (0..rows.len()).rev() {
            let vrow = vpos + offset as u64;
            let removed = self.seg.delete_one(vrow);
            debug_assert!(removed.is_some(), "预检后的插行逆删除必须成功");
        }
        self.rebind_redo_materialized(&materialized, block);
        true
    }

    /// 应用逆操作；完整校验失败时不修改数据。
    fn apply_inverse(&mut self, op: &Op) -> bool {
        match op {
            Op::SetCell { cell, old, .. } => {
                self.write_value(*cell, old);
                true
            }
            Op::SetCells { changes } => {
                for change in changes {
                    self.write_value(change.cell, &change.old);
                }
                true
            }
            Op::Paste { changes, inserted } => {
                if let Some(inserted) = inserted {
                    if !self.remove_inserted_block_for_undo(
                        inserted.vpos,
                        inserted.block,
                        &inserted.rows,
                    ) {
                        return false;
                    }
                }
                for change in changes {
                    self.write_value(change.cell, &change.old);
                }
                true
            }
            Op::InsertRows { vpos, block, rows } => {
                self.remove_inserted_block_for_undo(*vpos, *block, rows)
            }
            Op::RemoveRows { items } => {
                // 任何写入前验证全部插入块，后续失败不得留下前序源行恢复。
                if !self.seg.validate_restore_inserted(items) {
                    return false;
                }

                let visible = self.schema.visible().to_vec();
                let restored = items.iter().map(|item| match item {
                    DeletedRow::Src { snapshot, snapshot_row, .. }
                    | DeletedRow::Ins { snapshot, snapshot_row, .. }
                    | DeletedRow::Suspended { snapshot, snapshot_row, .. } => {
                        snapshot.read_row(*snapshot_row as usize)
                    }
                }).collect::<Result<Vec<_>, _>>();
                let Ok(restored) = restored else { return false };
                if items.iter().zip(&restored).any(|(item, cells)| {
                    matches!(item, DeletedRow::Src { .. } | DeletedRow::Suspended { .. })
                        && cells.len() != visible.len()
                }) {
                    return false;
                }
                // suspended 快照是逻辑列序；所有存储转换都在首次写入前完成。
                let restored = items.iter().zip(restored).map(|(item, cells)| {
                    if matches!(item, DeletedRow::Suspended { .. }) {
                        self.logical_rows_to_storage(vec![cells]).ok()?.pop()
                    } else {
                        Some(cells)
                    }
                }).collect::<Option<Vec<_>>>();
                let Some(restored) = restored else { return false };
                for (item, cells) in items.iter().zip(restored) {
                    match item {
                        DeletedRow::Src { vpos, rec, .. } => {
                            for (id, value) in visible.iter().zip(cells) {
                                self.overlay.insert((*rec, *id), value);
                            }
                            self.seg.tombstones.remove(rec);
                            let at = self.seg.split_at(*vpos);
                            self.seg.segs.insert(
                                at,
                                Seg { kind: SegKind::Src { start: *rec, len: 1 }, len: 1 },
                            );
                        }
                        DeletedRow::Ins { vpos, block, row, .. } => {
                            if !self.seg.restore_inserted(*vpos, *block, *row, cells) {
                                return false;
                            }
                        }
                        DeletedRow::Suspended { vpos, block, .. } => {
                            if !self.seg.restore_inserted(*vpos, *block, 0, cells) {
                                return false;
                            }
                        }
                    }
                }
                self.seg.compact();
                true
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
        let vrow = self.orig_vrow(vrow);
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
        for (_, kw, mode) in &mut spec.cols {
            // mode 2（严格·区分大小写）关键字保持原样大小写
            if *mode != 2 {
                *kw = kw.to_lowercase();
            }
        }
        spec.multi_text = spec.multi_text.to_lowercase();

        // 完全相同条件：直接复用上次结果（编辑/插删/撤销都会清空 last_spec）
        if self.filter.is_some() && self.last_spec.as_ref() == Some(&spec) {
            self.filter_stats = FilterStats { scanned_rows: 0 };
            return Ok(Some(self.filter_len()));
        }

        // 无条件 = 全命中（不扫字节）
        if spec.cols.is_empty() && spec.multi_text.is_empty() {
            let all: Vec<u32> = (0..total as u32).collect();
            self.filter = Some(all);
            self.last_spec = Some(spec);
            self.filter_stats = FilterStats { scanned_rows: total };
            return Ok(Some(total as u32));
        }

        // 多重筛选启用却未勾选任何列：任何行都不可能命中
        if !spec.multi_text.is_empty() && spec.multi_cols.is_empty() {
            self.filter = Some(Vec::new());
            self.last_spec = Some(spec);
            self.filter_stats = FilterStats { scanned_rows: 0 };
            return Ok(Some(0));
        }

        // 前缀收窄：同列结构 + 关键字逐字追加 → 只扫上次命中行
        let narrow = self
            .last_spec
            .as_ref()
            .filter(|prev| can_narrow(prev, &spec))
            .and_then(|_| self.filter.take());

        let source = match narrow.as_deref() {
            Some(rows) => RowSource::Subset(rows),
            None => RowSource::All(total),
        };
        let resolved = ResolvedFilterSpec::from_view(&spec, &self.schema);
        let chunk = 1u64 << 16;
        let nchunks = source.len().div_ceil(chunk).max(1) as usize;
        let parts: Vec<Option<ChunkOut>> = (0..nchunks)
            .into_par_iter()
            .map(|ci| {
                let start = ci as u64 * chunk;
                let end = (start + chunk).min(source.len());
                self.scan_chunk(&spec, &resolved, gate, source, start, end)
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
        let mut scanned = 0u64;
        for part in parts.into_iter().flatten() {
            scanned += part.rows;
            all.extend(part.hits);
        }

        let n = all.len() as u32;
        self.filter = Some(all);
        self.last_spec = Some(spec);
        self.filter_stats = FilterStats { scanned_rows: scanned };
        Ok(Some(n))
    }

    /// 扫描一个并行块（全量或收窄子集）。
    fn scan_chunk(
        &self,
        spec: &FilterSpec,
        resolved: &ResolvedFilterSpec,
        gate: Option<(&AtomicU64, u64)>,
        source: RowSource,
        start: u64,
        end: u64,
    ) -> Option<ChunkOut> {
        let mut hits = Vec::new();
        let mut scan = FieldScan::new_resolved(resolved, self.enc, &self.overlay, self.base_ncols);
        for i in start..end {
            if let Some((g, id)) = gate {
                if i & 0xFFF == 0 && g.load(Ordering::Relaxed) != id {
                    return None;
                }
            }
            let d = source.get(i) as u32;
            match self.seg.resolve(self.orig_vrow(d as u64)) {
                Some(ResolvedRow::Src(rec)) => {
                    // rec 已是物理记录号（段表统一寻址）；记录 0 需剥 BOM
                    let (s, e) = self
                        .src_span(rec)
                        .unwrap_or((0, 0));
                    if scan.record_matches(rec, &self.bytes()[s..e]) {
                        hits.push(d);
                    }
                }
                Some(ResolvedRow::Ins { .. }) => {
                    if self
                        .row_at(d as u64)
                        .is_some_and(|f| spec_matches(spec, &f))
                    {
                        hits.push(d);
                    }
                }
                None => {}
            }
        }
        Some(ChunkOut {
            hits,
            rows: end - start,
        })
    }

    /// 是否有生效的筛选结果。
    pub fn has_filter(&self) -> bool {
        self.filter.is_some()
    }

    /// 当前筛选命中数（无筛选时 0）。
    pub fn filter_len(&self) -> u32 {
        self.filter.as_ref().map_or(0, |f| f.len() as u32)
    }

    /// 清除筛选结果与收窄基准。
    pub fn clear_filter(&mut self) {
        self.filter = None;
        self.last_spec = None;
    }

    /// 筛选命中分页取行：行内容与各命中行的原始数据行号（0 基）成对下发。
    pub fn filter_page(&self, from: u32, count: u32) -> FilterPage {
        let Some(f) = &self.filter else { return FilterPage::default() };
        // 起点也要夹取：筛选切换时，筛选前排队的深页请求（如 from=19000）
        // 可能晚到；此时命中集可能只有几十行，直接切片会 panic 崩掉整个进程。
        let lo = (from as usize).min(f.len());
        let hi = (lo + count as usize).min(f.len());
        let mut page = FilterPage::default();
        for &d in &f[lo..hi] {
            // pairwise 推入：row_at 掉行时 rows/vrows 也不致错位
            if let Some(r) = self.row_at(d as u64) {
                page.vrows.push(self.orig_vrow(d as u64) as u32);
                page.rows.push(r);
            }
        }
        page
    }

    /// 命中序号 → 清筛后全量视图中的行号（取消筛选锚定用）。
    /// 命中集存的就是该值（未排序=原始 vrow；排序=排序显示位置），直接返回。
    pub fn filter_hit_view_row(&self, idx: u32) -> Option<u32> {
        self.filter.as_ref()?.get(idx as usize).copied()
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
            // 渐进索引期无插删行：vrow + 模式偏移 = 物理记录号
            for v in r0..r1 {
                let fields = self.src_fields(v as u32 + self.record_offset());
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

    /// 从 committed Schema 重放 pending IPC，得到纯逻辑预览结构。
    fn projected_schema(&self, ops: &[ColOp]) -> Result<Option<Arc<Schema>>, String> {
        if ops.is_empty() {
            return Ok(None);
        }
        let mut candidate = (*self.schema).clone();
        for op in ops {
            match op {
                ColOp::Insert { pos, name } => {
                    let pos = (*pos as usize).min(candidate.visible().len());
                    let pos = u32::try_from(pos).map_err(|_| "插入列位置越界")?;
                    candidate.insert(pos, name.clone())?;
                }
                ColOp::Delete { col } => {
                    if let Some(id) = candidate.view_id(*col) {
                        // pending 允许预览零列；apply 仍按既有契约拒绝。
                        candidate.delete_preview(id)?;
                    }
                }
                ColOp::Rename { col, name } => {
                    if let Some(id) = candidate.view_id(*col) {
                        candidate.rename(id, name.clone())?;
                    }
                }
            }
        }
        Ok(Some(Arc::new(candidate)))
    }

    /// 追加待执行列操作（渲染预览即时，物理重写延迟到 apply）。
    ///
    /// # Errors
    /// 后台索引未完成时拒绝（列映射会打乱索引期 vrow == 记录号的前提）。
    pub fn push_col_op(&mut self, op: ColOp) -> Result<(), String> {
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持列操作".into());
        }
        if self.order.is_some() {
            return Err("排序模式下暂不支持列操作，请先退出排序".into());
        }
        // 无列名模式列名是自动生成的，重命名没有意义
        if self.headerless && matches!(op, ColOp::Rename { .. }) {
            return Err("无列名模式不可重命名列".into());
        }
        let mut ops = self.pending_cols.clone();
        ops.push(op);
        let pending_schema = self.projected_schema(&ops)?;
        self.pending_cols = ops;
        self.pending_schema = pending_schema;
        self.filter = None; // 列映射变化，旧筛选结果作废
        self.last_spec = None;
        Ok(())
    }

    /// 撤销最近一次待执行列操作。
    pub fn pop_col_op(&mut self) {
        if self.pending_cols.is_empty() {
            self.pending_schema = None;
            return;
        }
        let ops = self.pending_cols[..self.pending_cols.len() - 1].to_vec();
        let pending_schema = self
            .projected_schema(&ops)
            .expect("已接受的 pending 操作必须能无损重放");
        self.pending_cols = ops;
        self.pending_schema = pending_schema;
    }

    /// 清空待执行列操作（用户放弃）。
    pub fn clear_col_ops(&mut self) {
        self.pending_cols.clear();
        self.pending_schema = None;
    }

    /// 切换「首行非列名」。
    ///
    /// 开启：表头行作为普通数据进入顶部，列名虚拟为 列1..N。
    /// 关闭：当前首行（含未保存编辑）提升为表头并从数据中移除。
    /// 渐进索引期没有段表，仅翻转模式偏移；关闭时把当前首行记为表头。
    ///
    /// # Parameters
    /// - `on`: true = 首行是数据；false = 首行是列名。
    ///
    /// # Errors
    /// 待执行列操作未清空 / 关闭时没有数据行时返回错误字符串。
    pub fn set_headerless(&mut self, on: bool) -> Result<(), String> {
        if on == self.headerless {
            return Ok(());
        }
        let next_history_generation = self.next_history_generation()?;
        if self.order.is_some() {
            return Err("排序模式下暂不支持切换首行语义，请先退出排序".into());
        }
        if !self.pending_cols.is_empty() {
            return Err("列操作待执行：请先执行或撤销列操作".into());
        }
        self.refresh_indexed();
        let mut renamed_schema = None;
        if self.partial.is_some() {
            if !on {
                if self.indexed_data_rows() == 0 {
                    return Err("没有数据行可作为列名".into());
                }
                // 记录 0 是当前首行（无列名模式 offset = 0）；覆盖层编辑一并带上
                let headers = self.src_fields(0);
                renamed_schema = Some(self.schema_with_headers(&headers)?);
                self.base_ncols = headers.len();
                self.base_headers = headers;
            }
            self.headerless = on;
        } else {
            if on {
                if self.base_headers.is_empty() {
                    return Err("没有列可作为列名".into());
                }
                // O(1)：原表头变成顶部插入行，可正常编辑/删除/筛选
                self.seg.insert_block(0, vec![self.base_headers.clone()]);
            } else {
                if self.seg.vrow_count() == 0 {
                    return Err("没有数据行可作为列名".into());
                }
                let Some(row) = self.row_at(0) else {
                    return Err("没有数据行可作为列名".into());
                };
                renamed_schema = Some(self.schema_with_headers(&row)?);
                self.base_ncols = row.len();
                self.base_headers = row;
                self.seg.delete_one(0);
            }
            self.headerless = on;
        }
        if let Some(schema) = renamed_schema {
            self.schema = Arc::new(schema);
        }
        self.pending_schema = None;
        // 开关是视图属性：筛选、收窄基准、撤销历史全部作废
        self.filter = None;
        self.last_spec = None;
        self.clear_history_at_fixed_point(next_history_generation);
        Ok(())
    }

    // ---- 序列化与落盘 ----

    /// 保存：流式重写原文件（覆盖层/插入/删除合并）。
    /// 进度回调 `(加权done, total, 阶段, 行数)`：加权把总体进度切给
    /// 流式 0-70% / 落盘 70-78% / 重建索引至 100%（见 PHASE_* 常量注释）。
    /// 保存后重建 mmap 与索引；撤销栈保留（操作按稳定记录号寻址，仍有效）。
    pub fn save<F: FnMut(u64, u64, u8, u64)>(
        &mut self,
        approval: Approval,
        progress: F,
    ) -> Result<SaveOutcome, String> {
        // 新建内存档没有磁盘路径：直存无目标，必须另存为。
        if !self.has_path() {
            return Err("未命名表格：请先另存为".into());
        }
        let dest = self.path.clone();
        self.save_to(&dest, false, approval, progress)
    }

    /// 另存为：把当前视图（覆盖层/插入/删除/排序合并）流式写到 `target`。
    /// 原文件保持不变；成功后会话身份切换到 `target`，后续保存写新文件。
    /// 写盘失败时原文件、当前映射与会话状态均不受影响。
    pub fn save_as<F: FnMut(u64, u64, u8, u64)>(
        &mut self,
        target: &Path,
        approval: Approval,
        progress: F,
    ) -> Result<SaveOutcome, String> {
        // 目标就是当前文件：按普通保存处理（同一路径无需“切换会话”）
        if target == self.path {
            return self.save(approval, progress);
        }
        self.save_to(target, true, approval, progress)
    }

    /// 当前逻辑行序对应的普通保存基线映射。
    fn build_baseline_transform(&self) -> Result<(BaselineTransform, usize), String> {
        let mut transform = BaselineTransform::default();
        transform.deleted_sources = self.seg.tombstones.clone();
        for (index, id) in self.schema.visible().iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| "保存列号溢出")?;
            transform.columns.insert(*id, index);
        }
        let mut new_rec = self.record_offset();
        for seg in &self.seg.segs {
            match seg.kind {
                SegKind::Src { start, len } => {
                    for offset in 0..len {
                        let old_rec = start.checked_add(offset).ok_or("源记录号溢出")?;
                        transform.push_surviving_source(old_rec, new_rec)?;
                        new_rec = new_rec.checked_add(1).ok_or("保存记录号溢出")?;
                    }
                }
                SegKind::Ins { block } => {
                    let rows = self.seg.blocks.get(block as usize)
                        .and_then(Option::as_ref).ok_or("插入块在保存前已失效")?;
                    for row in 0..rows.len() {
                        let row = u32::try_from(row).map_err(|_| "插入行号溢出")?;
                        if transform.inserted_rows.insert((block, row), new_rec).is_some() {
                            return Err("插入行保存映射重复".into());
                        }
                        new_rec = new_rec.checked_add(1).ok_or("保存记录号溢出")?;
                    }
                }
            }
        }

        // 仅为历史仍引用的已删除 Source 分配 suspended 块，避免按全部 tombstone 扩容。
        fn collect_cell(cell: &CellId, deleted: &BTreeSet<u32>, out: &mut BTreeSet<u32>) {
            if let RowId::Source(rec) = cell.row {
                if deleted.contains(&rec) {
                    out.insert(rec);
                }
            }
        }
        fn collect_op(op: &Op, deleted: &BTreeSet<u32>, out: &mut BTreeSet<u32>) {
            match op {
                Op::SetCell { cell, .. } => collect_cell(cell, deleted, out),
                Op::SetCells { changes } | Op::Paste { changes, .. } => {
                    for change in changes {
                        collect_cell(&change.cell, deleted, out);
                    }
                }
                Op::RemoveRows { items } => {
                    for item in items {
                        if let DeletedRow::Src { rec, .. } = item {
                            if deleted.contains(rec) {
                                out.insert(*rec);
                            }
                        }
                    }
                }
                Op::InsertRows { .. } => {}
            }
        }
        let mut suspended = BTreeSet::new();
        for entry in self.history.iter_undo() {
            collect_op(&entry.op, &transform.deleted_sources, &mut suspended);
        }
        for entry in self.history.iter_redo() {
            collect_op(&entry.op, &transform.deleted_sources, &mut suspended);
        }
        let mut next_block = u32::try_from(self.seg.blocks.len())
            .map_err(|_| "suspended 块号溢出")?;
        for rec in suspended {
            let block = next_block;
            next_block = next_block.checked_add(1).ok_or("suspended 块号溢出")?;
            transform.suspended_rows.insert(rec, block);
        }
        let block_slots = usize::try_from(next_block).map_err(|_| "suspended 块槽越界")?;
        Ok((transform, block_slots))
    }

    /// 将一条历史操作转换到新物理基线；旧 backing 的 Source 值迁移为 Text。
    fn transform_op_for_save(&self, op: &Op, transform: &BaselineTransform)
        -> Result<Op, String> {
        let map_cell = |cell: &CellId| -> Result<CellId, String> {
            if !transform.columns.contains_key(&cell.col) {
                return Err("历史列已不在保存结构中".into());
            }
            let row = match cell.row {
                RowId::Source(rec) => {
                    if let Some(mapped) = transform.map_source(rec) {
                        RowId::Source(mapped)
                    } else if let Some(block) = transform.suspended_rows.get(&rec) {
                        RowId::Inserted { block: *block, row: 0 }
                    } else {
                        return Err(format!("Source 记录 {rec} 无法映射到保存基线"));
                    }
                }
                RowId::Inserted { block, row } => {
                    if let Some(mapped) = transform.inserted_rows.get(&(block, row)) {
                        RowId::Source(*mapped)
                    } else {
                        RowId::Inserted { block, row }
                    }
                }
            };
            Ok(CellId { row, col: cell.col })
        };
        let map_value = |value: &ValueRef| -> Result<ValueRef, String> {
            match value {
                ValueRef::Text(text) => Ok(ValueRef::Text(Arc::clone(text))),
                ValueRef::Source { .. } => Ok(ValueRef::text(self.source_value_text(value)?)),
            }
        };
        let map_changes = |changes: &[CellChange]| -> Result<Vec<CellChange>, String> {
            changes.iter().map(|change| {
                Ok(CellChange {
                    cell: map_cell(&change.cell)?,
                    old: map_value(&change.old)?,
                    new: map_value(&change.new)?,
                })
            }).collect()
        };
        Ok(match op {
            Op::SetCell { cell, old, new } => Op::SetCell {
                cell: map_cell(cell)?, old: map_value(old)?, new: map_value(new)?,
            },
            Op::SetCells { changes } => Op::SetCells { changes: map_changes(changes)? },
            Op::Paste { changes, inserted } => Op::Paste {
                changes: map_changes(changes)?, inserted: inserted.clone(),
            },
            Op::InsertRows { .. } => op.clone(),
            Op::RemoveRows { items } => {
                let mut mapped = Vec::with_capacity(items.len());
                for item in items {
                    mapped.push(match item {
                        DeletedRow::Src { vpos, rec, snapshot, snapshot_row } => {
                            if let Some(mapped) = transform.map_source(*rec) {
                                DeletedRow::Src {
                                    vpos: *vpos, rec: mapped, snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                }
                            } else if let Some(block) = transform.suspended_rows.get(rec) {
                                DeletedRow::Suspended {
                                    vpos: *vpos, block: *block, snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                }
                            } else {
                                return Err(format!("删除历史 Source 记录 {rec} 无法映射"));
                            }
                        }
                        DeletedRow::Ins { vpos, block, row, snapshot, snapshot_row } => {
                            if let Some(rec) = transform.inserted_rows.get(&(*block, *row)) {
                                DeletedRow::Src {
                                    vpos: *vpos, rec: *rec, snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                }
                            } else {
                                DeletedRow::Ins {
                                    vpos: *vpos, block: *block, row: *row,
                                    snapshot: snapshot.clone(), snapshot_row: *snapshot_row,
                                }
                            }
                        }
                        DeletedRow::Suspended { vpos, block, snapshot, snapshot_row } => {
                            if let Some(rec) = transform.inserted_rows.get(&(*block, 0)) {
                                DeletedRow::Src {
                                    vpos: *vpos, rec: *rec, snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                }
                            } else {
                                DeletedRow::Suspended {
                                    vpos: *vpos, block: *block, snapshot: snapshot.clone(),
                                    snapshot_row: *snapshot_row,
                                }
                            }
                        }
                    });
                }
                Op::RemoveRows { items: mapped }
            }
        })
    }

    /// 读取 Source 历史值，并核对 backing 代际、记录区间和逻辑长度。
    fn source_value_text(&self, value: &ValueRef) -> Result<String, String> {
        let ValueRef::Source { row, col, offset, raw_len, utf8_len, index_epoch } = value else {
            return Err("非 Source 历史值".into());
        };
        if *index_epoch != self.index_epoch {
            return Err("Source 历史值所属 backing 代际已过期".into());
        }
        let RowId::Source(rec) = *row else {
            return Err("Source 历史值没有源记录身份".into());
        };
        let start = usize::try_from(*offset).map_err(|_| "Source 偏移越界")?;
        let raw_len = usize::try_from(*raw_len).map_err(|_| "Source 长度越界")?;
        let end = start.checked_add(raw_len).ok_or("Source 区间溢出")?;
        if self.src_span(rec) != Some((start, end)) {
            return Err("Source 历史值记录区间已变化".into());
        }
        let raw = self.bytes().get(start..end).ok_or("Source 区间越界")?;
        let Some(ColumnStorage::Source(slot)) = self.schema.storage(*col) else {
            return Err("Source 历史值列已不是物理源列".into());
        };
        let decoded = decode_slice(self.enc, raw);
        let fields = parse_record(&decoded, b',');
        let value = fields.get(slot as usize).cloned().unwrap_or_default();
        if value.len() as u64 != *utf8_len {
            return Err("Source 历史值逻辑长度已变化".into());
        }
        Ok(value)
    }

    /// 转换一个方向的完整历史栈，保留原始 revision_delta。
    fn transform_entries_for_save<'a>(
        &self,
        entries: impl Iterator<Item = &'a HistoryEntry<Op>>,
        transform: &BaselineTransform,
    ) -> Result<VecDeque<HistoryEntry<Op>>, String> {
        let mut converted = VecDeque::new();
        for entry in entries {
            let operation = match &entry.op {
                Op::SetCell { .. } => "编辑单元格",
                Op::SetCells { .. } => "批量编辑单元格",
                Op::Paste { .. } => "粘贴区域",
                Op::InsertRows { .. } => "插入行",
                Op::RemoveRows { .. } => "删除行",
            };
            let op = self.transform_op_for_save(&entry.op, transform)
                .map_err(|error| format!("{operation}：{error}"))?;
            let mut transformed = op.history_entry().map_err(history_cost_error)?;
            transformed.revision_delta = entry.revision_delta;
            converted.push_back(transformed);
        }
        Ok(converted)
    }

    /// 在旧数据与旧账本仍有效时完成全部可失败的历史转换和重建计费。
    fn prepare_history_for_save(&self) -> Result<PreparedSaveHistory, String> {
        let (transform, block_slots) = self.build_baseline_transform()?;
        let undo = self.transform_entries_for_save(self.history.iter_undo(), &transform)?;
        let redo = self.transform_entries_for_save(self.history.iter_redo(), &transform)?;
        let history = HistoryLedger::prepare_rebuild(
            undo,
            redo,
            self.history.stats().budget_bytes,
        ).map_err(history_cost_error)?;
        let mut blocks = Vec::new();
        blocks.try_reserve_exact(block_slots)
            .map_err(|_| "保存历史块槽内存不足".to_string())?;
        blocks.resize(block_slots, None);
        Ok(PreparedSaveHistory { history, blocks })
    }

    /// `save`/`save_as` 共用实现。
    ///
    /// @param dest 写盘目标（另存为的新路径，或当前文件路径）。
    /// @param switch_path 成功后是否把会话路径切到 `dest`（另存为为 true）。
    pub fn save_to<F: FnMut(u64, u64, u8, u64)>(
        &mut self,
        dest: &Path,
        switch_path: bool,
        approval: Approval,
        mut progress: F,
    ) -> Result<SaveOutcome, String> {
        if approval.allow_fixed_point
            && approval.preflight_id != Some(self.history_generation)
        {
            return Err("固定点预检已过期，请重试本次保存".into());
        }
        self.require_editable()?;
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，请稍候再保存".into());
        }
        // 无列名文件没有独立表头：0 行会写出 BOM-only/空文件，重开失败
        if self.headerless {
            if self.seg.vrow_count() == 0 {
                return Err("无列名模式需至少保留一行".into());
            }
            if self.view_ncols() == 0 {
                return Err("无列名模式需至少保留一列".into());
            }
        }
        let next_history_generation = self.next_history_generation()?;
        let next_index_epoch = self.index_epoch.checked_add(1).ok_or("索引代际溢出")?;
        // 整表重写数十秒：本线程降为背景优先级，让出 UI/系统调度。
        let _bg = bg::BackgroundGuard::enter();
        let sorted_order = self.order.clone();
        if sorted_order.is_some()
            && self.history.stats().undo_count + self.history.stats().redo_count > 0
            && !approval.allow_fixed_point
        {
            let request = self.save_fixed_point_request(
                FixedPointKind::SortMaterialization,
                "按当前排序保存并物化行序".into(),
            );
            return Ok(SaveOutcome {
                saved: false,
                fixed_point: Some(request),
                cancelled: false,
            });
        }
        let prepared_history = if sorted_order.is_some() {
            None
        } else {
            match self.prepare_history_for_save() {
                Ok(prepared) => Some(prepared),
                Err(operation) => {
                    if !approval.allow_fixed_point {
                        return Ok(SaveOutcome {
                            saved: false,
                            fixed_point: Some(self.save_fixed_point_request(
                                FixedPointKind::SaveTransform,
                                operation,
                            )),
                            cancelled: false,
                        });
                    }
                    None
                }
            }
        };
        // 临时文件放在目标同目录：换名是同卷原子操作，也保证另存为不碰原文件
        let tmp = tmp_path_for(dest);
        let out = fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        let mut w = BufWriter::new(out);
        if self.has_bom {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        // 表头模式先写列名；无列名模式段表第一行就是数据第一行。
        if !self.headerless {
            let header = self.view_headers();
            w.write_all(&encode_record(&header, b',', self.enc))
                .map_err(|e| e.to_string())?;
        }
        let total = self.seg.vrow_count();
        // 有覆盖层编辑的源行（透传判定的"不洁净"集；覆盖层通常极小）
        let edited: std::collections::HashSet<u32> =
            self.overlay.keys().map(|k| k.0).collect();
        let mut done = 0u64;
        let mut last_emit: Option<Instant> = None;
        if let Some(order) = &sorted_order {
            // 排序保存：按当前视图顺序逐行写出；物理顺序即所得。
            for &orig in order.iter() {
                match self.seg.resolve(orig as u64) {
                    Some(ResolvedRow::Src(rec)) => {
                        if edited.contains(&rec) {
                            let f = self.src_fields(rec);
                            w.write_all(&encode_record(&f, b',', self.enc))
                                .map_err(|e| e.to_string())?;
                        } else {
                            let (s, e) = self.src_span(rec).unwrap_or((0, 0));
                            w.write_all(&self.bytes()[s..e]).map_err(|e| e.to_string())?;
                            // 原末行可能没有换行；排到中间时必须补行尾。
                            if !self.bytes()[s..e].ends_with(b"\n") {
                                w.write_all(b"\r\n").map_err(|e| e.to_string())?;
                            }
                        }
                    }
                    Some(ResolvedRow::Ins { block, row }) => {
                        let f = self.ins_fields(block, row);
                        w.write_all(&encode_record(&f, b',', self.enc))
                            .map_err(|e| e.to_string())?;
                    }
                    None => {}
                }
                done += 1;
                if done & 0xF == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                    progress(stream_weighted(done), total, PHASE_STREAM, done);
                }
            }
            progress(stream_weighted(done), total, PHASE_STREAM, done);
            w.flush().map_err(|e| e.to_string())?;
            drop(w);
            progress(flushed_weighted(total), total, PHASE_FLUSH, done);
            let prepared_reopen = self.prepare_reopen(&tmp)?;
            self.replace_with_retry(&tmp, dest)?;
            if switch_path {
                self.path = dest.to_path_buf();
            }
            progress(renamed_weighted(total), total, PHASE_FLUSH, done);
            self.commit_reopen(prepared_reopen);
            self.index_epoch = next_index_epoch;
            // 排序物化仍是固定点；成功保存本身重新建立干净保存点。
            self.order = None;
            self.history.clear();
            self.fixed_point = true;
            self.revision = 0;
            self.savepoint_detached = false;
            self.history_generation = next_history_generation;
            progress(total, total, PHASE_REINDEX, total);
            return Ok(SaveOutcome {
                saved: true,
                fixed_point: None,
                cancelled: false,
            });
        }
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
                        for (row_index, _) in rows.iter().enumerate() {
                            let row = u32::try_from(row_index).map_err(|_| "插入行号越界")?;
                            let f = self.ins_fields(block, row);
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
        let prepared_reopen = self.prepare_reopen(&tmp)?;
        self.replace_with_retry(&tmp, dest)?;
        if switch_path {
            self.path = dest.to_path_buf();
        }
        progress(renamed_weighted(total), total, PHASE_FLUSH, done);
        self.commit_reopen(prepared_reopen);
        self.index_epoch = next_index_epoch;
        if let Some(prepared) = prepared_history {
            self.seg.blocks = prepared.blocks;
            self.history.commit_rebuild(prepared.history);
        } else {
            self.history.clear();
            self.fixed_point = true;
        }
        self.revision = 0;
        self.savepoint_detached = false;
        self.history_generation = next_history_generation; // 内存状态与磁盘一致
        progress(total, total, PHASE_REINDEX, total); // 收尾必到满，前端进度条归零前显示 100%
        Ok(SaveOutcome {
            saved: true,
            fixed_point: None,
            cancelled: false,
        })
    }

    /// 执行待执行的列操作：流式重写（物理列映射），完成后重建会话。
    /// 此操作不可撤销（撤销栈清空）。进度回调契约同 `save`。
    pub fn apply_col_ops<F: FnMut(u64, u64, u8, u64)>(
        &mut self,
        mut progress: F,
    ) -> Result<(), String> {
        if self.order.is_some() {
            return Err("排序模式下暂不支持列操作，请先退出排序".into());
        }
        self.refresh_indexed();
        if self.is_indexing() {
            return Err("文件仍在索引中，暂不支持列操作".into());
        }
        if self.pending_cols.is_empty() {
            return Ok(());
        }
        let next_history_generation = self.next_history_generation()?;
        let history_budget = self.history.stats().budget_bytes;
        // 视图→物理映射（apply 后 view == 物理，映射只是本次重写用）
        let ncols = self.view_ncols();
        let total = self.seg.vrow_count();
        // 无列名模式没有独立表头；零行/零列都会写出无法重开的文件
        if self.headerless {
            if total == 0 {
                return Err("无列名模式需至少保留一行".into());
            }
            if ncols == 0 {
                return Err("无列名模式需至少保留一列".into());
            }
        } else if ncols == 0 {
            return Err("没有列：无法执行列操作".into());
        }
        // 整表物理重写数十秒：本线程降为背景优先级，让出 UI/系统调度。
        let _bg = bg::BackgroundGuard::enter();
        let new_headers = self.view_headers();

        let dest = self.path.clone();
        let tmp = tmp_path_for(&dest);
        // 无路径内存档：写入内存缓冲（不落临时文件），apply 后内存内重建。
        let target = if self.has_path() {
            ColWriteTarget::File(
                fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?,
            )
        } else {
            ColWriteTarget::Mem(Vec::new())
        };
        let mut w = BufWriter::new(target);
        if self.has_bom {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        // 无列名模式不写独立表头：第一行记录就是数据
        if !self.headerless {
            w.write_all(&encode_record(&new_headers, b',', self.enc))
                .map_err(|e| e.to_string())?;
        }
        let mut done = 0u64;
        let mut last_emit: Option<Instant> = None;
        for s in &self.seg.segs {
            match s.kind {
                SegKind::Src { start, len } => {
                    for rec in start..start + len {
                        let mapped = self.map_fields_to_view(&self.src_fields(rec));
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
                        for (row_index, _) in rows.iter().enumerate() {
                            let row = u32::try_from(row_index).map_err(|_| "插入行号越界")?;
                            let mapped = self.map_fields_to_view(&self.ins_fields(block, row));
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
        // into_inner 内含 flush；取出写出目标以分流落盘/内存两条重建路径。
        let target = w.into_inner().map_err(|e| e.to_string())?;
        progress(flushed_weighted(total), total, PHASE_FLUSH, done);
        // 全量重建：列结构变化，编辑层与撤销历史一并失效；保持首行语义
        let mut fresh = match target {
            ColWriteTarget::File(_) => {
                self.replace_with_retry(&tmp, &dest)?;
                progress(renamed_weighted(total), total, PHASE_FLUSH, done);
                Self::open_with(&self.path, self.headerless)?
            }
            // 无路径内存档：缓冲直接重建会话，全程不写盘。
            ColWriteTarget::Mem(buf) => Self::from_bytes(buf, self.headerless)?,
        };
        fresh.schema = Arc::new(Schema::from_headers(new_headers.clone())?);
        fresh.pending_schema = None;
        fresh.base_headers = new_headers;
        fresh.base_ncols = ncols as usize;
        fresh.history = HistoryLedger::new(history_budget);
        fresh.history_generation = next_history_generation;
        fresh.fixed_point = true;
        *self = fresh;
        progress(total, total, PHASE_REINDEX, total); // 收尾必到满
        Ok(())
    }

    /// 重新映射原文件并重建行索引（不动 Segment/覆盖层/撤销栈）。
    /// 用于保存换名失败后的读路径恢复。
    fn remap(&mut self) -> Result<(), String> {
        debug_assert!(self.has_path(), "remap 需要磁盘路径（无路径内存档不可达）");
        let t0 = Instant::now();
        let file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        self.cancel.store(true, Ordering::Relaxed); // 防御：旧索引线程不要再用旧映射
        self.partial = None;
        self.idx = build_index(bytes);
        self.backing = Backing::Mmap(mmap);
        self.index_ms = t0.elapsed().as_millis() as u64;
        self.index_sync = true;
        Ok(())
    }

    /// 用写好的临时文件替换原文件。
    ///
    /// 换名前先摘除本进程的映射句柄（自己的只读句柄不挡换名，也不给
    /// 错误排查添变量），失败按 200ms 间隔重试 5 次（覆盖杀软/索引器等
    /// 瞬时占用）；仍失败则映回原文件保持会话可用（编辑层/撤销栈不动），
    /// 临时文件原样保留，错误信息给出路径供手动恢复。
    fn replace_with_retry(&mut self, tmp: &Path, dest: &Path) -> Result<(), String> {
        // 有路径 mmap 档：占位匿名映射换出旧映射、关闭文件句柄（保存前已
        // refresh_indexed，无后台线程共享），1 字节开销；换名成功后由重开
        // 路径重建后备。无路径 Ram 档的内存字节是唯一副本，绝不在此丢弃。
        if !self.is_ram_backed() {
            let dummy = memmap2::MmapMut::map_anon(1)
                .and_then(|m| m.make_read_only())
                .map_err(|e| e.to_string())?;
            drop(std::mem::replace(&mut self.backing, Backing::Mmap(Arc::new(dummy))));
        }
        let mut last: Option<std::io::Error> = None;
        for _ in 0..5 {
            match fs::rename(tmp, dest) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
        // 换名失败：有路径档映回原文件恢复读路径（seg/覆盖层/撤销栈原样保留）；
        // 无路径内存档后备未动、会话原样可用，直接报错即可。
        if self.has_path() {
            if let Err(re) = self.remap() {
                return Err(format!(
                    "写入目标文件失败：{}\n且原文件重新映射失败：{}",
                    last.map(|e| e.to_string()).unwrap_or_default(),
                    re
                ));
            }
        }
        Err(format!(
            "写入目标文件失败：{}\n新文件已完整写在临时文件里（数据未丢失）：{}\n\
             可关闭占用目标文件的程序后重试保存，或手动把该临时文件改名为目标文件。",
            last.map(|e| e.to_string()).unwrap_or_default(),
            tmp.display()
        ))
    }

    /// 在临时路径完成 mmap、索引和预期记录数校验，失败时尚未替换目标。
    fn prepare_reopen(&self, path: &Path) -> Result<PreparedReopen, String> {
        let t0 = Instant::now();
        let n_data = self.seg.vrow_count();
        let offset = self.record_offset();
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let mmap = Arc::new(unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        });
        let bytes: &[u8] = &mmap;
        let idx = build_index(bytes);
        let expected = n_data.checked_add(offset as u64).ok_or("写入后记录数溢出")?;
        if idx.n_records() as u64 != expected {
            return Err("写入后行数校验失败".into());
        }
        let n_data = u32::try_from(n_data).map_err(|_| "写入后行数溢出")?;
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        Ok(PreparedReopen {
            backing: Backing::Mmap(mmap),
            idx,
            index_ms: t0.elapsed().as_millis() as u64,
            total_bytes: meta.len(),
            modified,
            offset,
            n_data,
        })
    }

    /// 目标换名后的唯一状态提交；不再执行 IO、分配或可失败身份转换。
    fn commit_reopen(&mut self, prepared: PreparedReopen) {
        self.backing = prepared.backing;
        self.idx = prepared.idx;
        self.partial = None;
        self.index_ms = prepared.index_ms;
        self.index_sync = true;
        self.total_bytes = prepared.total_bytes;
        self.backing_path = self.path.clone();
        self.seg = Segments::new(prepared.offset, prepared.n_data);
        self.overlay.clear();
        self.filter = None;
        self.last_spec = None;
        self.base_size = prepared.total_bytes;
        self.base_mtime = prepared.modified;
    }

}

/// 无列名模式的虚拟列名：列1..列N。
fn generic_headers(ncols: usize) -> Vec<String> {
    (1..=ncols).map(|i| format!("列{i}")).collect()
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
    for (c, kw, mode) in &spec.cols {
        let hit = fields.get(*c as usize).is_some_and(|f| match *mode {
            2 => f == kw,                                // 严格·区分大小写
            1 => f.to_lowercase() == *kw,                // 严格·不分大小写
            _ => f.to_lowercase().contains(kw.as_str()), // 包含
        });
        if !hit {
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

/// 列操作重写写出目标：有路径档流式落临时文件；无路径内存档写内存缓冲。
enum ColWriteTarget {
    File(fs::File),
    Mem(Vec<u8>),
}

impl std::io::Write for ColWriteTarget {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File(f) => f.write(buf),
            Self::Mem(v) => {
                v.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Mem(_) => Ok(()),
        }
    }
}
