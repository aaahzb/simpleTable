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

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use memchr::{memchr, memchr_iter};
use rayon::prelude::*;
use std::time::{Duration, Instant};

// ============================================================
// 长操作友好度：低优先级守卫 + 进度节流
// ============================================================

/// 进度回调最小发送间隔：重写期间每 65536 行回调一次，1550 万行 ≈ 237 次
/// IPC 事件；磁盘 IO 被拖慢时前端会被进度事件淹没、渲染管线反而饿死。
/// 限速到 ~12 次/秒，肉眼看仍是连续走动，IPC 压力降到 1/50 以下。
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
        bf.save(|done, _total| prog = prog.max(done as u32)).unwrap();
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
        bf.save(|_, _| {}).unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.meta().encoding, "gbk");
        assert_eq!(bf2.rows(0, 1)[0], vec!["张三", "上海"]);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn big_col_ops_pending_blocks_edits_then_apply() {
        let p = tmp_csv("colops", &people_csv());
        let mut bf = BigFile::open(&p).unwrap();
        // pending：视图立即多出列
        bf.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() });
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
        bf.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() });
        bf.push_col_op(ColOp::Rename { col: 0, name: "姓名".into() });
        bf.push_col_op(ColOp::Delete { col: 2 }); // 删原 age
        bf.apply_col_ops(|_, _| {}).unwrap();
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
        let err = bf.save(|_, _| {}).unwrap_err();
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
        bf.save(|_, _| {}).unwrap();
        t.join().unwrap();
        let bf2 = BigFile::open(&p).unwrap();
        assert_eq!(bf2.rows(0, 1)[0][0], "Bob", "新文件应落盘生效");
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
}

/// 大文件编辑会话。
pub struct BigFile {
    path: PathBuf,
    mmap: memmap2::Mmap,
    enc: Encoding,
    idx: RawIndex,
    base_ncols: usize,
    base_headers: Vec<String>,
    seg: Segments,
    /// 源记录编辑层：稳定身份 → 新值（插入行直接改块，不经此层）。
    overlay: HashMap<(u32, u32), String>,
    pending_cols: Vec<ColOp>,
    filter: Option<Vec<u32>>,
    undo_stack: Vec<Op>,
    redo_stack: Vec<Op>,
    base_size: u64,
    base_mtime: std::time::SystemTime,
}

impl BigFile {
    /// 打开并建索引（秒级：三趟并行扫描，不解析字段内容）。
    ///
    /// # Errors
    /// 文件不可读/为空/无记录时返回错误字符串。
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = fs::File::open(path)
            .map_err(|e| format!("打开失败：{}（{}）", path.display(), e))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.len() == 0 {
            return Err("空文件".into());
        }
        let mmap = unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        };
        let enc = sniff_encoding(&mmap);
        let idx = build_index(&mmap);
        if idx.n_records() == 0 {
            return Err("文件中没有记录".into());
        }
        let (hs, he) = idx.record_span(mmap.len(), 0);
        let mut headers = parse_record(&decode_slice(enc, &mmap[hs..he]), b',');
        if enc == Encoding::Utf8 {
            if let Some(first) = headers.first_mut() {
                if let Some(stripped) = first.strip_prefix('\u{FEFF}') {
                    *first = stripped.to_string();
                }
            }
        }
        let n_data = idx.n_records() - 1;
        let base_ncols = headers.len();
        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            enc,
            idx,
            base_ncols,
            base_headers: headers,
            seg: Segments::new(n_data as u32),
            overlay: HashMap::new(),
            pending_cols: Vec::new(),
            filter: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            base_size: meta.len(),
            base_mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        })
    }

    pub fn meta(&self) -> BigMeta {
        BigMeta {
            rows: self.seg.vrow_count(),
            cols: self.view_ncols(),
            headers: self.view_headers(),
            encoding: match self.enc {
                Encoding::Utf8 => "utf-8".into(),
                Encoding::Gbk => "gbk".into(),
            },
            pending_cols: !self.pending_cols.is_empty(),
            can_undo: !self.undo_stack.is_empty(),
            can_redo: !self.redo_stack.is_empty(),
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
    fn src_fields(&self, rec: u32) -> Vec<String> {
        let (s, e) = self.idx.record_span(self.mmap.len(), rec as usize + 1);
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

    /// 取 vrow 的视图行（pending 期做列映射）。
    fn row_at(&self, vrow: u64) -> Option<Vec<String>> {
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
        let total = self.seg.vrow_count();
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
        let resolved = self.seg.resolve(vrow).ok_or("行号越界")?;
        let cell = match resolved {
            ResolvedRow::Src(rec) => CellId::Src { rec, col },
            ResolvedRow::Ins { block, row } => CellId::Ins { block, row, col },
        };
        let old = self.cell_value(cell);
        if old == value {
            return Ok(());
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
        let _ = len;
        Ok(())
    }

    /// 删除一组 vrow（升序传入；快照超限的删除不可撤销）。
    pub fn remove_rows(&mut self, mut vrows: Vec<u64>) -> Result<u32, String> {
        self.require_editable()?;
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
        let restored = self.apply_inverse(&op);
        self.redo_stack.push(restored);
        true
    }

    /// 重做。
    pub fn redo(&mut self) -> bool {
        let Some(op) = self.redo_stack.pop() else { return false };
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

    /// 执行筛选：并行扫描 vrow 空间，命中列表存引擎侧，返回命中数。
    pub fn run_filter(&mut self, spec: &FilterSpec) -> Result<u32, String> {
        self.require_editable()?;
        let total = self.seg.vrow_count();
        if total > u32::MAX as u64 {
            return Err("文件过大（超过 42.9 亿行），暂不支持筛选".into());
        }
        let cum = self.seg_cum_lens();
        let chunk = 1u64 << 16;
        let nchunks = total.div_ceil(chunk).max(1) as usize;
        let parts: Vec<Vec<u32>> = (0..nchunks)
            .into_par_iter()
            .map(|ci| {
                let start = ci as u64 * chunk;
                let end = (start + chunk).min(total);
                let mut out = Vec::new();
                for v in start..end {
                    if let Some(fields) = self.row_at_fast(&cum, v) {
                        if spec_matches(spec, &fields) {
                            out.push(v as u32);
                        }
                    }
                }
                out
            })
            .collect();
        let mut all: Vec<u32> = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
        for mut p in parts {
            all.append(&mut p);
        }
        let n = all.len() as u32;
        self.filter = Some(all);
        Ok(n)
    }

    /// 是否有生效的筛选结果。
    pub fn has_filter(&self) -> bool {
        self.filter.is_some()
    }

    /// 当前筛选命中数（无筛选时 0）。
    pub fn filter_len(&self) -> u32 {
        self.filter.as_ref().map_or(0, |f| f.len() as u32)
    }

    /// 清除筛选结果。
    pub fn clear_filter(&mut self) {
        self.filter = None;
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
        if r1 < r0 || r1 > self.seg.vrow_count() {
            return Err("复制范围越界".into());
        }
        let cum = self.seg_cum_lens();
        let mut out = String::new();
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
    pub fn push_col_op(&mut self, op: ColOp) {
        self.pending_cols.push(op);
        self.filter = None; // 列映射变化，旧筛选结果作废
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

    /// 保存：流式重写原文件（覆盖层/插入/删除合并），进度回调 (done, total)。
    /// 保存后重建 mmap 与索引；撤销栈保留（操作按稳定记录号寻址，仍有效）。
    pub fn save<F: FnMut(u64, u64)>(&mut self, mut progress: F) -> Result<(), String> {
        self.require_editable()?;
        // 整表重写数十秒：本线程降为背景优先级，让出 UI/系统调度。
        let _bg = bg::BackgroundGuard::enter();
        let tmp = tmp_path_for(&self.path);
        let out = fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败：{e}"))?;
        let mut w = BufWriter::new(out);
        if self.enc == Encoding::Utf8 {
            w.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        let header = self.view_headers();
        w.write_all(&encode_record(&header, b',', self.enc))
            .map_err(|e| e.to_string())?;
        let total = self.seg.vrow_count();
        let mut done = 0u64;
        let mut last_emit: Option<Instant> = None;
        for s in &self.seg.segs {
            match s.kind {
                SegKind::Src { start, len } => {
                    for rec in start..start + len {
                        let f = self.src_fields(rec);
                        w.write_all(&encode_record(&f, b',', self.enc))
                            .map_err(|e| e.to_string())?;
                        done += 1;
                        if done % 65536 == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                            progress(done, total);
                        }
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
                            if done % 65536 == 0 {
                                progress(done, total);
                            }
                        }
                    }
                }
            }
        }
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        progress(total, total); // 收尾必发一次，前端进度条归零前显示 100%
        self.replace_with_retry(&tmp)?;
        self.reopen_after_write()?;
        Ok(())
    }

    /// 执行待执行的列操作：流式重写（物理列映射），完成后重建会话。
    /// 此操作不可撤销（撤销栈清空）。
    pub fn apply_col_ops<F: FnMut(u64, u64)>(
        &mut self,
        mut progress: F,
    ) -> Result<(), String> {
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
        if self.enc == Encoding::Utf8 {
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
                        if done % 65536 == 0 && progress_gate(&mut last_emit, PROGRESS_MIN_GAP) {
                            progress(done, total);
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
                            if done % 65536 == 0 {
                                progress(done, total);
                            }
                        }
                    }
                }
            }
        }
        w.flush().map_err(|e| e.to_string())?;
        drop(w);
        progress(total, total); // 收尾必发一次，前端进度条归零前显示 100%
        self.replace_with_retry(&tmp)?;

        // 全量重建：列结构变化，编辑层与撤销历史一并失效
        let mut fresh = Self::open(&self.path)?;
        fresh.base_headers = new_headers;
        fresh.base_ncols = ncols as usize;
        *self = fresh;
        Ok(())
    }

    /// 重新映射原文件并重建行索引（不动 Segment/覆盖层/撤销栈）。
    /// 用于保存换名失败后的读路径恢复。
    fn remap(&mut self) -> Result<(), String> {
        let file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let mmap = unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        };
        self.idx = build_index(&mmap);
        self.mmap = mmap;
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
        drop(std::mem::replace(&mut self.mmap, dummy)); // 旧映射解除、文件句柄关闭
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
        let n_data = self.seg.vrow_count();
        let file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let mmap = unsafe {
            memmap2::MmapOptions::new().map(&file).map_err(|e| e.to_string())?
        };
        let idx = build_index(&mmap);
        if idx.n_records() as u64 != n_data + 1 {
            return Err("写入后行数校验失败".into());
        }
        self.mmap = mmap;
        self.idx = idx;
        self.seg = Segments::new(n_data as u32);
        self.overlay.clear();
        self.filter = None;
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
