use super::materialize::{
    chunk_spans, commit_ram, count_spans_parallel, generate_spans_parallel,
    has_line_split_boundary, should_parallelize, MaterializationInput, MaterializeFault,
    PrepareMaterialization, MIN_ROWS_PER_CHUNK, PARALLEL_MATERIALIZATIONS, PARALLEL_MIN_ROWS,
};
use super::serialize::{
    serialize_document, CountingSink, RamRegionSink, RowSpan, SerializationContext,
    SerializationStats, SerializationView,
};
use crate::order::Perm;
use crate::ram_region::{thread_reserved_bytes, RamRegion};
use super::{
    tmp_path_for, Backing, BigFile, ColOp, FilterSpec, SaveRamEvent, SaveRamPhase, Schema,
    RAM_COMMITTED_ERROR,
};
use crate::history::{Approval, HistoryStats};
use crate::ram_load::{LoadFault, LoadOptions};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

/// 创建独立的合成 CSV，避免测试接触真实业务文件。
fn fixture_path(name: &str, extension: &str, bytes: &[u8]) -> PathBuf {
    let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "simpletable_materialize_{name}_{}_{id}.{extension}",
        std::process::id(),
    ));
    fs::write(&path, bytes).unwrap();
    path
}

/// 打开带一条单元格编辑的持久内存文档。
fn edited_memory_document(name: &str) -> (PathBuf, BigFile) {
    let path = fixture_path(
        name,
        "csv",
        b"name,age,city\r\nAlice,30,NY\r\nBob,40,SF\r\n",
    );
    let mut document = BigFile::open(&path).unwrap();
    assert!(document
        .enter_memory_mode_for_test(&AtomicBool::new(false), |_| {})
        .unwrap());
    document
        .set_cell(0, 1, "31".into(), Approval::deny())
        .unwrap();
    (path, document)
}

#[derive(Debug, PartialEq)]
struct MaterializationSnapshot {
    rows: Vec<Vec<String>>,
    headers: Vec<String>,
    history: HistoryStats,
    revision: i64,
    generation: u64,
    epoch: u64,
    path: PathBuf,
    backing_path: PathBuf,
    base_size: u64,
    base_mtime: std::time::SystemTime,
    filter: Option<FilterSpec>,
    pending_cols: Vec<ColOp>,
}

impl BigFile {
    fn index_capacities(&self) -> (usize, usize) {
        (self.idx.line_off.capacity(), self.idx.rec_lines.capacity())
    }

    /// 捕获准备失败时必须逐字段保持不变的会话状态。
    fn materialization_snapshot(&self) -> MaterializationSnapshot {
        MaterializationSnapshot {
            rows: self.rows(
                0,
                u32::try_from(self.seg.vrow_count()).expect("测试夹具行数必须适合 u32"),
            ),
            headers: self.view_headers(),
            history: self.history_stats(),
            revision: self.revision,
            generation: self.history_generation,
            epoch: self.index_epoch,
            path: self.path.clone(),
            backing_path: self.backing_path.clone(),
            base_size: self.base_size,
            base_mtime: self.base_mtime,
            filter: self.last_spec.clone(),
            pending_cols: self.pending_cols.clone(),
        }
    }
}

fn position_of(
    events: &[SaveRamEvent],
    predicate: impl Fn(&SaveRamEvent) -> bool,
) -> usize {
    events.iter().position(predicate).unwrap()
}

/// 构造指定数据行数的 CSV 夹具，供分块计划矩阵使用。
fn rows_fixture(name: &str, rows: u64) -> (PathBuf, BigFile) {
    let mut csv = String::from("name,age\r\n");
    for row in 0..rows {
        csv.push_str(&format!("row{row},{row}\r\n"));
    }
    let path = fixture_path(name, "csv", csv.as_bytes());
    let document = BigFile::open(&path).unwrap();
    (path, document)
}

#[test]
fn chunk_spans_cover_every_row_without_empty_chunks() {
    for total in [0u64, 1, 8_191, 8_192, 65_536, 65_537, 100_000, 1_000_000] {
        for workers in [0usize, 1, 2, 3, 4, 16] {
            let spans = chunk_spans(total, workers);
            if total == 0 {
                assert!(spans.is_empty(), "total=0 不应产生块");
                continue;
            }
            assert!(!spans.is_empty(), "total={total} workers={workers}");
            assert_eq!(spans[0].start, 0);
            assert_eq!(spans.last().unwrap().end, total);
            let mut cursor = 0u64;
            for span in &spans {
                assert_eq!(span.start, cursor, "块必须首尾相接");
                assert!(span.len() > 0, "不允许空块");
                cursor = span.end;
            }
            assert!(
                spans.len() <= workers.max(1) * 4,
                "块数不得超过 4×并行度：{} vs {workers}",
                spans.len()
            );
            if total >= MIN_ROWS_PER_CHUNK {
                assert!(
                    spans.iter().all(|span| span.len() >= MIN_ROWS_PER_CHUNK),
                    "多块时每块不得少于 {MIN_ROWS_PER_CHUNK} 行：{spans:?}"
                );
            } else {
                assert_eq!(spans.len(), 1, "小表只能有一块");
            }
            let min = spans.iter().map(|span| span.len()).min().unwrap();
            let max = spans.iter().map(|span| span.len()).max().unwrap();
            assert!(max - min <= 1, "块必须均分：{spans:?}");
        }
    }
}

#[test]
fn parallel_selection_requires_workers_rows_and_no_injected_fault() {
    let (path, document) = rows_fixture("parallel-selection", PARALLEL_MIN_ROWS);
    let total = document.seg.vrow_count();
    assert_eq!(total, PARALLEL_MIN_ROWS);
    assert!(should_parallelize(
        &document,
        total,
        8,
        MaterializeFault::None,
        false
    ));
    assert!(!should_parallelize(
        &document,
        total,
        1,
        MaterializeFault::None,
        false
    ));
    assert!(!should_parallelize(
        &document,
        total,
        0,
        MaterializeFault::None,
        false
    ));
    assert!(!should_parallelize(
        &document,
        total - 1,
        8,
        MaterializeFault::None,
        false
    ));
    for fault in [
        MaterializeFault::CountAfter(1),
        MaterializeFault::CommitAt(0),
        MaterializeFault::Reserve,
    ] {
        assert!(
            !should_parallelize(&document, total, 8, fault, false),
            "故障注入必须回退顺序路径：{fault:?}"
        );
    }
    assert!(!has_line_split_boundary(&document));
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn line_split_boundary_detected_only_for_missing_final_newline_with_later_inserts() {
    // 末条记录缺换行：其后没有插入行时仍是完整的物理行边界。
    let path = fixture_path(
        "split-boundary-tail-insert",
        "csv",
        b"name,age\r\nAlice,30\r\nBob,40",
    );
    let mut document = BigFile::open(&path).unwrap();
    assert!(!has_line_split_boundary(&document));
    document.insert_rows(2, None, 1, Approval::deny()).unwrap();
    assert!(
        has_line_split_boundary(&document),
        "缺换行的末条记录后还有插入行时允许回退"
    );
    drop(document);
    fs::remove_file(path).ok();

    // 插入行在缺换行记录之前：最后一段以 EOF 结束，安全。
    let path = fixture_path(
        "split-boundary-head-insert",
        "csv",
        b"name,age\r\nAlice,30\r\nBob,40",
    );
    let mut document = BigFile::open(&path).unwrap();
    document.insert_rows(0, None, 1, Approval::deny()).unwrap();
    assert!(!has_line_split_boundary(&document));
    drop(document);
    fs::remove_file(path).ok();

    // 末条记录有换行时任何插入布局都安全。
    let path = fixture_path(
        "split-boundary-newline",
        "csv",
        b"name,age\r\nAlice,30\r\nBob,40\r\n",
    );
    let mut document = BigFile::open(&path).unwrap();
    document.insert_rows(2, None, 1, Approval::deny()).unwrap();
    assert!(!has_line_split_boundary(&document));
    drop(document);
    fs::remove_file(path).ok();

    // 达到并行阈值的缺换行布局：排序分支补分隔符，必须保持可并行；
    // 非排序分支会切断物理行，必须回退顺序路径。
    let mut csv = String::from("name,age\r\n");
    for row in 0..PARALLEL_MIN_ROWS {
        csv.push_str(&format!("row{row},{row}\r\n"));
    }
    csv.truncate(csv.len() - 2);
    let path = fixture_path("split-boundary-scaled", "csv", csv.as_bytes());
    let mut document = BigFile::open(&path).unwrap();
    document
        .insert_rows(document.seg.vrow_count(), None, 1, Approval::deny())
        .unwrap();
    assert!(has_line_split_boundary(&document));
    let total = document.seg.vrow_count();
    assert!(should_parallelize(
        &document,
        total,
        8,
        MaterializeFault::None,
        true
    ));
    assert!(!should_parallelize(
        &document,
        total,
        8,
        MaterializeFault::None,
        false
    ));
    drop(document);
    fs::remove_file(path).ok();
}

/// 把 `total` 行尽量均分为不超过 `chunks` 个非空区间，用于强制分块矩阵。
fn split_rows(total: u64, chunks: usize) -> Vec<RowSpan> {
    let count = usize::try_from(total.min(chunks as u64)).unwrap_or(1).max(1);
    let base = total / count as u64;
    let rem = total % count as u64;
    let mut spans = Vec::with_capacity(count);
    let mut start = 0u64;
    for index in 0..count {
        let len = base + u64::from((index as u64) < rem);
        spans.push(RowSpan {
            start,
            end: start + len,
        });
        start += len;
    }
    spans
}

/// 并行计数矩阵夹具：覆盖 BOM、GBK、引号多行、缺末尾换行与纯文本。
fn counting_fixture_cases() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let gbk = encoding_rs::GBK
        .encode("name,note\r\nAlice,北京\r\nBob,上海\r\nCara,广州\r\n")
        .0
        .into_owned();
    vec![
        (
            "count-crlf",
            "csv",
            b"name,note\r\nAlice,a\r\nBob,b\r\nCara,c\r\n".to_vec(),
        ),
        (
            "count-bom",
            "csv",
            b"\xEF\xBB\xBFname,note\nAlice,a\nBob,b\nCara,c\n".to_vec(),
        ),
        (
            "count-quotes",
            "csv",
            b"name,note\r\nAlice,\"a,b\nline2\"\r\nBob,b\r\nCara,c\r\n".to_vec(),
        ),
        (
            "count-no-final",
            "csv",
            b"name,age\r\nAlice,30\r\nBob,40\r\nCara,50".to_vec(),
        ),
        ("count-gbk", "csv", gbk),
        (
            "count-text",
            "txt",
            "北京\n\nAlice\n鲍勃\n".as_bytes().to_vec(),
        ),
    ]
}

#[test]
fn parallel_count_matches_sequential_stats_across_chunk_matrix() {
    for (name, extension, bytes) in counting_fixture_cases() {
        let path = fixture_path(name, extension, &bytes);
        let document = BigFile::open(&path).unwrap();
        let headers = document.view_headers();
        let view = SerializationView {
            document: &document,
            schema: &document.schema,
            saved_headers: &headers,
            sorted: None,
        };
        let mut baseline_sink = CountingSink::new();
        let baseline = serialize_document(view, &mut baseline_sink, |_, _| {}).unwrap();
        let total = document.seg.vrow_count();
        assert!(total >= 2, "{name}");
        for chunks in [1usize, 2, 3, 7] {
            let spans = split_rows(total, chunks);
            let (per_chunk, combined) =
                count_spans_parallel(view, &spans, 3, |_, _| {}).unwrap();
            assert_eq!(per_chunk.len(), spans.len(), "{name} chunks={chunks}");
            assert_eq!(combined, baseline, "{name} chunks={chunks}");
            let mut merged: Option<super::serialize::SerializationStats> = None;
            for stats in &per_chunk {
                merged = Some(match merged {
                    None => *stats,
                    Some(previous) => previous.combine(*stats).unwrap(),
                });
            }
            assert_eq!(merged.unwrap(), baseline, "{name} chunks={chunks} 分块合并");
        }
        drop(document);
        fs::remove_file(path).ok();
    }
}

#[test]
fn parallel_count_progress_is_monotonic_and_reaches_total() {
    let (path, document) = rows_fixture("count-progress", 200_000);
    let headers = document.view_headers();
    let view = SerializationView {
        document: &document,
        schema: &document.schema,
        saved_headers: &headers,
        sorted: None,
    };
    let spans = chunk_spans(document.seg.vrow_count(), 8);
    assert!(spans.len() > 1);
    let samples = Mutex::new(Vec::new());
    let (_, combined) = count_spans_parallel(view, &spans, 4, |done, total| {
        samples.lock().unwrap().push((done, total));
    })
    .unwrap();
    assert_eq!(combined.rows, 200_000);
    let samples = samples.into_inner().unwrap();
    assert!(!samples.is_empty(), "至少要有一次进度回调");
    let mut previous = 0u64;
    for (done, total) in &samples {
        assert_eq!(*total, 200_000);
        assert!(*done >= previous, "进度必须单调：{samples:?}");
        previous = *done;
    }
    assert_eq!(
        samples.last().unwrap().0, 200_000,
        "终态必须等于总行数：{samples:?}"
    );
    drop(document);
    fs::remove_file(path).ok();
}

/// 用给定分块矩阵比较顺序与并行生成的字节与统计。
fn assert_parallel_generation_matches(view: SerializationView<'_>, label: &str) {
    let context = SerializationContext::new(view);
    let total = context.total_rows();
    assert!(total > 0, "{label} 夹具必须含数据行");
    let mut counting = CountingSink::new();
    let baseline_stats = serialize_document(view, &mut counting, |_, _| {}).unwrap();
    let mut baseline_sink =
        RamRegionSink::new(RamRegion::reserve(baseline_stats.bytes as usize).unwrap());
    let written = serialize_document(view, &mut baseline_sink, |_, _| {}).unwrap();
    assert_eq!(written, baseline_stats);
    let baseline = baseline_sink.finish().unwrap();

    for chunks in [1usize, 2, 3, 7] {
        let spans = split_rows(total, chunks);
        let (per_chunk, combined) =
            count_spans_parallel(view, &spans, 3, |_, _| {}).unwrap();
        assert_eq!(combined, baseline_stats, "{label} chunks={chunks} 计数合并");
        let region = RamRegion::reserve(combined.bytes as usize).unwrap();
        let body =
            generate_spans_parallel(view, &spans, &per_chunk, region, 3, |_, _| {}).unwrap();
        assert_eq!(
            body.as_slice(),
            baseline.as_slice(),
            "{label} chunks={chunks} 生成字节"
        );
    }
}

#[test]
fn parallel_generation_matches_sequential_for_every_format_and_chunk_partition() {
    for (name, extension, bytes) in counting_fixture_cases() {
        let path = fixture_path(name, extension, &bytes);
        let document = BigFile::open(&path).unwrap();
        let headers = document.view_headers();
        assert_parallel_generation_matches(
            SerializationView {
                document: &document,
                schema: &document.schema,
                saved_headers: &headers,
                sorted: None,
            },
            name,
        );
        drop(document);
        fs::remove_file(path).ok();
    }
}

#[test]
fn parallel_generation_matches_sequential_for_edits_inserts_columns_and_sorting() {
    let mut csv = String::from("name,age,city\r\n");
    for row in 0..20u32 {
        csv.push_str(&format!("row{row},{row},city{row}\r\n"));
    }
    let path = fixture_path("parallel-edits", "csv", csv.as_bytes());
    let mut document = BigFile::open(&path).unwrap();
    document
        .set_cell(0, 1, "北 京,8".into(), Approval::deny())
        .unwrap();
    document
        .set_cell(5, 0, "quoted \"x\"".into(), Approval::deny())
        .unwrap();
    document
        .insert_rows(
            7,
            Some(vec![vec!["ins".into(), "1".into(), "z".into()]]),
            0,
            Approval::deny(),
        )
        .unwrap();
    document.push_col_op(ColOp::Delete { col: 1 }).unwrap();
    document
        .push_col_op(ColOp::Insert {
            pos: 1,
            name: "memo".into(),
        })
        .unwrap();
    let candidate = document.pending_schema.clone().unwrap();
    let headers = candidate
        .visible()
        .iter()
        .map(|id| candidate.resolve(*id).unwrap().header.clone())
        .collect::<Vec<_>>();
    let view = SerializationView {
        document: &document,
        schema: &candidate,
        saved_headers: &headers,
        sorted: None,
    };
    assert_parallel_generation_matches(view, "编辑+插入行+列操作");

    let total = document.seg.vrow_count();
    let perm = Perm::new((0..total as u32).rev().collect()).unwrap();
    let sorted_view = SerializationView {
        sorted: Some(&perm),
        ..view
    };
    assert_parallel_generation_matches(sorted_view, "编辑+插入行+列操作+排序");

    drop(document);
    fs::remove_file(path).ok();
}

/// 构造达到并行阈值、带一条编辑的持久内存文档。
fn large_memory_document(name: &str, rows: u64) -> (PathBuf, BigFile) {
    let mut csv = String::from("name,age,city\r\n");
    for row in 0..rows {
        csv.push_str(&format!("row{row},{row},city{row}\r\n"));
    }
    let path = fixture_path(name, "csv", csv.as_bytes());
    let mut document = BigFile::open(&path).unwrap();
    assert!(document
        .enter_memory_mode_for_test(&AtomicBool::new(false), |_| {})
        .unwrap());
    document
        .set_cell(0, 1, "31".into(), Approval::deny())
        .unwrap();
    (path, document)
}

/// 按同一次冻结视图做整篇顺序序列化，作为并行物化的字节参考。
fn sequential_reference(document: &BigFile, schema: &Schema, headers: &[String]) -> Vec<u8> {
    let view = SerializationView {
        document,
        schema,
        saved_headers: headers,
        sorted: None,
    };
    let mut counting = CountingSink::new();
    let stats = serialize_document(view, &mut counting, |_, _| {}).unwrap();
    let mut sink = RamRegionSink::new(RamRegion::reserve(stats.bytes as usize).unwrap());
    serialize_document(view, &mut sink, |_, _| {}).unwrap();
    sink.finish().unwrap().as_slice().to_vec()
}

#[test]
fn parallel_prepare_matches_sequential_body_stats_and_reserves_one_body() {
    let (path, mut document) = large_memory_document("parallel-prepare", 70_000);
    let headers = document.view_headers();
    let expected = sequential_reference(&document, &document.schema, &headers);
    let before = thread_reserved_bytes();
    let spawned_before = PARALLEL_MATERIALIZATIONS.with(|count| count.get());
    let input = MaterializationInput::save_current(&document, Approval::deny())
        .unwrap()
        .with_test_workers(4);
    let PrepareMaterialization::Ready(prepared) = document
        .prepare_materialization_for_test(input, |_| {})
        .unwrap()
    else {
        panic!("内存保存不应触发固定点");
    };
    assert_eq!(
        PARALLEL_MATERIALIZATIONS.with(|count| count.get()),
        spawned_before + 1,
        "达到阈值的保存必须走并行物化"
    );
    assert_eq!(prepared.stats.bytes as usize, expected.len());
    assert_eq!(prepared.body.as_slice(), expected.as_slice());
    // 新增私有内存恰好是一份新正文：没有临时块缓冲或额外区域。
    assert_eq!(
        thread_reserved_bytes() - before,
        prepared.stats.bytes as usize
    );
    drop(prepared);
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn parallel_columns_prepare_matches_sequential_body() {
    let (path, mut document) = large_memory_document("parallel-columns", 70_000);
    // 删除未编辑的尾列，避免历史变换因编辑列消失而要求固定点。
    document.push_col_op(ColOp::Delete { col: 2 }).unwrap();
    document
        .push_col_op(ColOp::Insert {
            pos: 1,
            name: "memo".into(),
        })
        .unwrap();
    let candidate = document.pending_schema.clone().unwrap();
    let headers = candidate
        .visible()
        .iter()
        .map(|id| candidate.resolve(*id).unwrap().header.clone())
        .collect::<Vec<_>>();
    let expected = sequential_reference(&document, &candidate, &headers);
    let spawned_before = PARALLEL_MATERIALIZATIONS.with(|count| count.get());
    // 列操作本身超出撤销预算时，确认固定点后仍走同一物化路径。
    let approval = Approval::allow(document.history_generation);
    let input = MaterializationInput::columns(&document, approval)
        .unwrap()
        .with_test_workers(4);
    let PrepareMaterialization::Ready(prepared) = document
        .prepare_materialization_for_test(input, |_| {})
        .unwrap()
    else {
        panic!("列操作物化不应触发固定点");
    };
    assert_eq!(
        PARALLEL_MATERIALIZATIONS.with(|count| count.get()),
        spawned_before + 1
    );
    assert_eq!(prepared.body.as_slice(), expected.as_slice());
    drop(prepared);
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn parallel_prepare_faults_fall_back_to_sequential_and_preserve_state() {
    for (name, fault, expected) in [
        ("parallel-count", MaterializeFault::CountAfter(1), "计数"),
        ("parallel-commit", MaterializeFault::CommitAt(0), "提交"),
        ("parallel-reserve", MaterializeFault::Reserve, "预留"),
    ] {
        let (path, mut document) = large_memory_document(name, PARALLEL_MIN_ROWS);
        let before = document.materialization_snapshot();
        let original = document.backing_identity();
        let spawned_before = PARALLEL_MATERIALIZATIONS.with(|count| count.get());
        let input = MaterializationInput::save_current(&document, Approval::deny())
            .unwrap()
            .with_fault(fault)
            .with_test_workers(4);
        let error = match document.prepare_materialization_for_test(input, |_| {}) {
            Ok(_) => panic!("{name}: 故障应使准备失败"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "{name}: {error}");
        assert_eq!(
            PARALLEL_MATERIALIZATIONS.with(|count| count.get()),
            spawned_before,
            "{name}: 故障必须回退顺序路径"
        );
        assert_eq!(document.backing_identity(), original);
        assert_eq!(document.materialization_snapshot(), before);
        drop(document);
        fs::remove_file(path).ok();
    }
}

#[test]
fn parallel_prepare_progress_is_monotonic_and_terminal() {
    const ROWS: u64 = 200_000;
    let (path, mut document) = large_memory_document("parallel-progress", ROWS);
    let mut events: Vec<(u64, u64, u8, u64)> = Vec::new();
    let mut collect = |done: u64, total: u64, phase: u8, rows: u64| {
        events.push((done, total, phase, rows));
    };
    let input = MaterializationInput::save_current(&document, Approval::deny())
        .unwrap()
        .with_progress(&mut collect)
        .with_test_workers(4);
    let PrepareMaterialization::Ready(prepared) = document
        .prepare_materialization_for_test(input, |_| {})
        .unwrap()
    else {
        panic!("内存保存不应触发固定点");
    };
    drop(prepared);
    drop(collect);
    assert!(!events.is_empty());
    for phase in [0u8, 1u8] {
        let phase_events: Vec<_> = events.iter().filter(|event| event.2 == phase).collect();
        assert!(!phase_events.is_empty(), "缺少阶段 {phase} 的进度");
        let mut previous = 0u64;
        for event in &phase_events {
            assert_eq!(event.1, ROWS);
            assert!(event.0 >= previous, "阶段 {phase} 进度必须单调：{phase_events:?}");
            previous = event.0;
        }
        let last = phase_events.last().unwrap();
        assert_eq!(last.0, ROWS, "阶段 {phase} 未走到末尾");
        assert_eq!(last.3, ROWS);
    }
    // 并行轮询按 80 ms 门限聚合，事件数远小于行数。
    assert!(events.len() < (ROWS as usize) / 10, "进度事件未节流");
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn materialization_two_passes_match_and_reserve_after_history_and_index() {
    let (path, mut document) = edited_memory_document("materialize-two-pass");
    let original = document.backing_identity();
    let old_capacities = document.index_capacities();
    let events = Mutex::new(Vec::new());
    let input = MaterializationInput::save_current(&document, Approval::deny()).unwrap();
    let PrepareMaterialization::Ready(prepared) = document
        .prepare_materialization_for_test(input, |event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
    else {
        panic!("unexpected fixed point")
    };
    assert_eq!(prepared.body.len() as u64, prepared.stats.bytes);
    assert_eq!(prepared.stats.rows, document.seg.vrow_count());
    assert!(document.index_capacities().0 >= prepared.stats.line_capacity);
    assert!(document.index_capacities().1 >= prepared.stats.record_capacity);
    assert!(old_capacities.0 <= document.index_capacities().0);
    assert_eq!(
        document.backing_identity(),
        original,
        "提交前旧 RAM 不得被接管"
    );

    let events = events.into_inner().unwrap();
    let count = position_of(&events, |event| {
        matches!(
            event,
            SaveRamEvent::Phase(SaveRamPhase::CountFinished { .. })
        )
    });
    let history = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::HistoryPrepared))
    });
    let index = position_of(&events, |event| {
        matches!(
            event,
            SaveRamEvent::Phase(SaveRamPhase::IndexPrepared { .. })
        )
    });
    let reserve = position_of(&events, |event| {
        matches!(
            event,
            SaveRamEvent::Phase(SaveRamPhase::NewReserveBegin { .. })
        )
    });
    assert!(
        count < history && history < index && index < reserve,
        "{events:?}"
    );
    drop(prepared);
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn materialization_count_or_second_pass_fault_preserves_complete_old_state() {
    for (name, fault, expected) in [
        ("count", MaterializeFault::CountAfter(1), "计数"),
        // 顺序 RamRegion 只在实际提交水位检查故障；0 必定命中第二遍首次页提交。
        ("commit", MaterializeFault::CommitAt(0), "提交"),
    ] {
        let (path, mut document) = edited_memory_document(name);
        let filter = FilterSpec {
            cols: vec![(1, "31".into(), 0)],
            multi_cols: Vec::new(),
            multi_text: String::new(),
        };
        assert_eq!(document.run_filter(&filter).unwrap(), 1);
        let before = document.materialization_snapshot();
        let original = document.backing_identity();
        let input = MaterializationInput::save_current(&document, Approval::deny())
            .unwrap()
            .with_fault(fault);
        let error = match document.prepare_materialization_for_test(input, |_| {}) {
            Ok(_) => panic!("{name}: 故障应使准备失败"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "{name}: {error}");
        assert_eq!(document.backing_identity(), original);
        assert_eq!(document.materialization_snapshot(), before);
        drop(document);
        fs::remove_file(path).ok();
    }
}

#[test]
fn materialization_commit_installs_body_reuses_index_and_rebuilds_filter() {
    let (path, mut document) = edited_memory_document("materialize-commit");
    let filter = FilterSpec {
        cols: vec![(1, "31".into(), 0)],
        multi_cols: Vec::new(),
        multi_text: String::new(),
    };
    assert_eq!(document.run_filter(&filter).unwrap(), 1);
    assert!(document.last_spec.is_some());
    let before = document.materialization_snapshot();
    let old_identity = document.backing_identity();
    let events = Mutex::new(Vec::new());
    let input = MaterializationInput::save_current(&document, Approval::deny()).unwrap();
    let PrepareMaterialization::Ready(prepared) = document
        .prepare_materialization_for_test(input, |event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
    else {
        panic!("unexpected fixed point")
    };
    let expected_key = prepared.content_key.clone();
    // SerializationStats.rows 不含表头，RawIndex::n_records 则包含表头物理记录。
    let expected_records = prepared.stats.rows
        + u64::from(!document.headerless && !document.format.is_text_lines());
    let expected_line_capacity = prepared.stats.line_capacity;
    let expected_record_capacity = prepared.stats.record_capacity;
    let expected_bytes = prepared.stats.bytes;

    commit_ram(&mut document, prepared, &|event| {
        events.lock().unwrap().push(event)
    });
    assert_ne!(document.backing_identity(), old_identity);
    assert_eq!(document.rows(0, 1)[0], ["Alice", "31", "NY"]);
    assert_eq!(document.idx.n_records() as u64, expected_records);
    assert!(document.idx.line_off.capacity() >= expected_line_capacity);
    assert!(document.idx.rec_lines.capacity() >= expected_record_capacity);
    assert_eq!(document.ram_content_key, Some(expected_key));
    assert!(document.disk_body_dirty);
    assert_eq!(document.path, before.path);
    assert_eq!(document.backing_path, before.backing_path);
    assert_eq!(document.base_size, before.base_size);
    assert_eq!(document.base_mtime, before.base_mtime);
    assert_eq!(document.total_bytes, expected_bytes);
    assert!(
        document.last_spec.is_none()
            && document.stable_hits.is_none()
            && document.display_hits.is_none()
    );
    assert_eq!(
        document.run_filter(&filter).unwrap(),
        1,
        "必须按新正文重新求值"
    );

    let events = events.into_inner().unwrap();
    let ready = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))
    });
    let released = position_of(&events, |event| {
        matches!(
            event,
            SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. })
        )
    });
    assert!(ready < released, "{events:?}");
    drop(document);
    fs::remove_file(path).ok();
}

#[test]
fn memory_columns_materialize_ram_without_touching_disk() {
    let (path, mut bf) = edited_memory_document("columns-ram-only");
    let disk_before = fs::read(&path).unwrap();
    let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
    let old_ram = bf.backing_identity();
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    let events = Mutex::new(Vec::new());
    let outcome = bf
        .apply_col_ops_observed(Approval::deny(), &|event| {
            events.lock().unwrap().push(event)
        })
        .unwrap();
    assert!(outcome.changed && bf.modified() && bf.disk_body_dirty);
    assert!(bf.pending_cols.is_empty() && bf.pending_schema.is_none());
    assert_ne!(bf.backing_identity(), old_ram);
    assert_eq!(bf.view_headers(), ["name", "empty", "age", "city"]);
    assert_eq!(bf.rows(0, 1)[0], ["Alice", "", "31", "NY"]);
    assert_eq!(fs::read(&path).unwrap(), disk_before);
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        mtime_before
    );
    let events = events.into_inner().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. })
            | SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. })
    )));
    drop(bf);
    fs::remove_file(path).ok();
}

#[test]
fn memory_columns_fault_preserves_live_schema_and_pending_preview() {
    let (path, mut bf) = edited_memory_document("columns-pending-atomic");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    let before = bf.materialization_snapshot();
    let pending_schema = bf.pending_schema.clone();
    let old_ram = bf.backing_identity();
    let input = MaterializationInput::columns(&bf, Approval::deny())
        .unwrap()
        // 小夹具首次实际页提交水位为 0；16 不会触发顺序区域故障。
        .with_fault(MaterializeFault::CommitAt(0));
    let error = match bf.prepare_materialization_for_test(input, |_| {}) {
        Ok(_) => panic!("第二遍故障应使准备失败"),
        Err(error) => error,
    };
    assert!(error.contains("提交"), "{error}");
    assert_eq!(bf.materialization_snapshot(), before);
    assert_eq!(bf.pending_schema, pending_schema);
    assert_eq!(bf.backing_identity(), old_ram);
    drop(bf);
    fs::remove_file(path).ok();
}

#[test]
fn non_memory_columns_keep_schema_only_behavior() {
    let fixture = b"name,age,city\r\nAlice,30,NY\r\nBob,40,SF\r\n";
    let old_path = fixture_path("columns-old-entry", "csv", fixture);
    let new_path = fixture_path("columns-observed-entry", "csv", fixture);
    let mut expected = BigFile::open(&old_path).unwrap();
    let mut actual = BigFile::open(&new_path).unwrap();
    expected
        .push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    actual
        .push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    let original = actual.backing_identity();
    let events = Mutex::new(Vec::new());

    let expected_outcome = expected.apply_col_ops(Approval::deny()).unwrap();
    let actual_outcome = actual
        .apply_col_ops_observed(Approval::deny(), &|event| {
            events.lock().unwrap().push(event)
        })
        .unwrap();
    assert_eq!(actual_outcome, expected_outcome);
    assert!(matches!(actual.backing, Backing::Mmap(_)));
    assert_eq!(actual.backing_identity(), original);
    assert_eq!(actual.view_headers(), expected.view_headers());
    assert_eq!(actual.rows(0, 2), expected.rows(0, 2));
    assert!(events.into_inner().unwrap().iter().all(|event| !matches!(
        event,
        SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })
            | SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. })
            | SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. })
    )));
    drop((expected, actual));
    fs::remove_file(old_path).ok();
    fs::remove_file(new_path).ok();
}

#[test]
fn general_single_empty_insert_preserves_raw_records_and_never_reloads_temp() {
    let mut source = vec![0xEF, 0xBB, 0xBF];
    source.extend_from_slice(
        b"name,age,note\r\n\"Alice\",30,\"x,y\"\r\n\"bob\nb\",25,\"say \"\"hi\"\"\"\r\nCarol,35,plain",
    );
    let path = fixture_path("general-raw-insert", "csv", &source);
    let mut file = BigFile::open(&path).unwrap();
    assert!(file
        .enter_memory_mode_for_test(&AtomicBool::new(false), |_| {})
        .unwrap());
    file.push_col_op(ColOp::Insert { pos: 1, name: "memo".into() })
        .unwrap();
    let events = Mutex::new(Vec::new());
    assert!(file
        .apply_col_ops_observed(Approval::deny(), &|event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
        .changed);
    let expected: Vec<u8> = [
        &b"\xEF\xBB\xBF"[..],
        &b"name,memo,age,note\r\n\"Alice\",,30,\"x,y\"\r\n\"bob\nb\",,25,\"say \"\"hi\"\"\"\r\nCarol,,35,plain"[..],
    ]
    .concat();
    assert_eq!(
        file.backing.bytes(),
        expected,
        "列操作后 RAM 必须保持原始字段风格"
    );
    assert_eq!(fs::read(&path).unwrap(), source, "列操作尚未写盘");
    let ram_identity = file.backing_identity();
    assert!(file
        .save_to_observed(
            &path,
            false,
            Approval::deny(),
            LoadOptions {
                chunk_bytes: 16,
                workers: 1,
                fault: LoadFault::ReadAfter(0),
            },
            |_, _, _, _| {},
            |event| events.lock().unwrap().push(event),
        )
        .unwrap()
        .saved);
    assert_eq!(file.backing_identity(), ram_identity);
    assert_eq!(fs::read(&path).unwrap(), expected);
    let events = events.into_inner().unwrap();
    let ready = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. }))
    });
    let released = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::OldRamReleased { .. }))
    });
    let disk = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin { .. }))
    });
    let committed = position_of(&events, |event| {
        matches!(event, SaveRamEvent::Phase(SaveRamPhase::DiskCommitted { .. }))
    });
    assert!(ready < released && released < disk && disk < committed, "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })
            ))
            .count(),
        1
    );
    drop(file);
    fs::remove_file(path).ok();
}

#[test]
fn general_insert_irregular_records_uses_same_serializer_without_temp_reload() {
    let source = b"a,b\r\n1\r\n2,3,4\r\n".to_vec();
    let path = fixture_path("general-irregular-insert", "csv", &source);
    let mut file = BigFile::open(&path).unwrap();
    assert!(file
        .enter_memory_mode_for_test(&AtomicBool::new(false), |_| {})
        .unwrap());
    file.push_col_op(ColOp::Insert { pos: 1, name: "x".into() })
        .unwrap();
    let events = Mutex::new(Vec::new());
    assert!(file
        .apply_col_ops_observed(Approval::deny(), &|event| {
            events.lock().unwrap().push(event)
        })
        .unwrap()
        .changed);
    assert_eq!(file.backing.bytes(), b"a,x,b\r\n1,,\r\n2,,3\r\n");
    assert_eq!(fs::read(&path).unwrap(), source);
    let identity = file.backing_identity();
    assert!(file
        .save_to_observed(
            &path,
            false,
            Approval::deny(),
            LoadOptions {
                chunk_bytes: 16,
                workers: 1,
                fault: LoadFault::ReadAfter(0),
            },
            |_, _, _, _| {},
            |event| events.lock().unwrap().push(event),
        )
        .unwrap()
        .saved);
    assert_eq!(file.backing_identity(), identity);
    assert_eq!(fs::read(&path).unwrap(), b"a,x,b\r\n1,,\r\n2,,3\r\n");
    assert_eq!(
        events
            .into_inner()
            .unwrap()
            .iter()
            .filter(|event| matches!(
                event,
                SaveRamEvent::Phase(SaveRamPhase::NewRamReady { .. })
            ))
            .count(),
        1,
        "保存必须复用列操作已经物化的 RAM"
    );
    drop(file);
    fs::remove_file(path).ok();
}

#[test]
fn general_insert_second_pass_fault_preserves_pending_and_old_ram() {
    let (path, mut bf) = edited_memory_document("general-insert-second-pass-fault");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    let before = bf.materialization_snapshot();
    let pending = bf.pending_schema.clone();
    let identity = bf.backing_identity();
    let disk = fs::read(&path).unwrap();
    let input = MaterializationInput::columns(&bf, Approval::deny())
        .unwrap()
        // 小夹具首次真实提交水位为 0；64 KiB 不会在此夹具触发故障。
        .with_fault(MaterializeFault::CommitAt(0));
    let error = match bf.prepare_materialization_for_test(input, |_| {}) {
        Ok(_) => panic!("第二遍故障应使准备失败"),
        Err(error) => error,
    };
    assert!(error.contains("提交"), "{error}");
    assert_eq!(bf.materialization_snapshot(), before);
    assert_eq!(bf.pending_schema, pending);
    assert_eq!(bf.backing_identity(), identity);
    assert_eq!(fs::read(&path).unwrap(), disk);
    drop(bf);
    fs::remove_file(path).ok();
}

#[test]
fn general_insert_rename_fault_keeps_materialized_ram_dirty() {
    let (path, mut bf) = edited_memory_document("general-insert-rename-fault");
    bf.push_col_op(ColOp::Insert { pos: 1, name: "empty".into() })
        .unwrap();
    bf.apply_col_ops_observed(Approval::deny(), &|_| {})
        .unwrap();
    let identity = bf.backing_identity();
    let history = bf.history_stats();
    let fixed_point = bf.fixed_point;
    let row_count = u32::try_from(bf.seg.vrow_count()).unwrap();
    let rows = bf.rows(0, row_count);
    let disk = fs::read(&path).unwrap();
    let target = std::env::temp_dir().join(format!(
        "st_general_insert_rename_directory_{}",
        std::process::id()
    ));
    fs::create_dir(&target).unwrap();
    let error = bf
        .save_to_observed(
            &target,
            true,
            Approval::deny(),
            LoadOptions::default(),
            |_, _, _, _| {},
            |_| {},
        )
        .unwrap_err();
    assert!(error.starts_with(RAM_COMMITTED_ERROR), "{error}");
    assert!(bf.disk_body_dirty && bf.modified());
    assert_eq!(
        bf.backing_identity(),
        identity,
        "重试写盘不得重新物化或读临时文件"
    );
    assert_eq!(bf.history_stats(), history);
    assert_eq!(bf.fixed_point, fixed_point);
    assert_eq!(bf.rows(0, row_count), rows);
    assert_eq!(bf.path, path);
    assert_eq!(bf.backing_path, path);
    assert_eq!(fs::read(&path).unwrap(), disk);
    fs::remove_file(tmp_path_for(&target)).ok();
    fs::remove_dir(&target).ok();
    drop(bf);
    fs::remove_file(path).ok();
}

#[test]
fn materialization_progress_is_throttled_and_reaches_phase_boundaries() {
    const ROWS: usize = 20_000;
    let mut csv = String::from("name,age\r\n");
    for row in 0..ROWS {
        csv.push_str(&format!("row{row},{row}\r\n"));
    }
    let path = fixture_path("progress-throttle", "csv", csv.as_bytes());
    let mut document = BigFile::open(&path).unwrap();
    assert!(document
        .enter_memory_mode_for_test(&AtomicBool::new(false), |_| {})
        .unwrap());
    document
        .set_cell(0, 1, "999".into(), Approval::deny())
        .unwrap();
    let mut events: Vec<(u64, u64, u8, u64)> = Vec::new();
    let mut collect = |done: u64, total: u64, phase: u8, rows: u64| {
        events.push((done, total, phase, rows));
    };
    let input = MaterializationInput::save_current(&document, Approval::deny())
        .unwrap()
        .with_progress(&mut collect);
    let prepared = document
        .prepare_materialization_for_test(input, |_| {})
        .unwrap();
    let PrepareMaterialization::Ready(prepared) = prepared else {
        panic!("内存保存不应触发固定点");
    };
    drop(prepared);
    // 释放对 events 的独占借用后才能读回收集结果。
    drop(collect);
    // 逐行回调会发出两遍共 2 * ROWS 条事件；按时间门限节流后必须远低于行数。
    assert!(
        events.len() < ROWS / 10,
        "进度事件未节流：{} 条",
        events.len()
    );
    // 每个阶段都必须以一条 done == total 的边界事件收尾，前端才能显示 100%。
    for phase in [0u8, 1u8] {
        let last = events
            .iter()
            .rev()
            .find(|event| event.2 == phase)
            .unwrap_or_else(|| panic!("缺少阶段 {phase} 的进度事件"));
        assert_eq!(last.0, ROWS as u64, "阶段 {phase} 未走到末尾：{last:?}");
        assert_eq!(last.1, ROWS as u64, "阶段 {phase} 的总数不对：{last:?}");
    }
    drop(document);
    fs::remove_file(path).ok();
}
