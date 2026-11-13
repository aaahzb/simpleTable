//! 双遍 RAM 物化、历史准备和只做状态移动的 RAM 提交。

use super::serialize::{
    serialize_document, CountingSink, RamRegionSink, RowSpan, SerializationContext,
    SerializationStats, SerializationView, SliceSink,
};
use super::{
    effective_undo_budget_bytes, line_is_blank, Backing, BigFile,
    DocumentFormat, Encoding, FileStamp, FixedPointKind, HistoryDecision, Op, PreparedSaveHistory,
    QuoteMachine, RamBuffer, RawIndex, RowOrder, SaveOutcome, SaveRamEvent, SaveRamPhase, Schema,
    SchemaChange, SegKind, Segments, SaveRebuildPolicy,
};
use crate::history::{Approval, FixedPointRequest};
use crate::order::Perm;
use crate::ram_region::{FinishedRam, RamRegion, RangeSpan};
use memchr::memchr_iter;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(test)]
use super::memory_save_tests::DiskWriteFault;

/// 当前 RAM 正文对应的逻辑内容身份；revision 和磁盘保存点不参与比较。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RamContentKey {
    pub history_generation: u64,
    pub schema: Arc<Schema>,
    pub order: RowOrder,
    pub output_encoding: Encoding,
    pub format: DocumentFormat,
    pub headerless: bool,
    pub has_bom: bool,
}

/// 物化准备成功或需要用户确认固定点。
pub(super) enum PrepareMaterialization {
    Ready(PreparedMaterialization),
    FixedPoint(FixedPointRequest),
}

/// 仅测试使用的受控故障；正常路径始终为 None。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum MaterializeFault {
    #[default]
    None,
    CountAfter(u64),
    CommitAt(u64),
    Reserve,
}

/// 保存当前逻辑文档，或以候选 Schema 执行列操作。
enum MaterializationOperation {
    SaveCurrent,
    /// 保留给物化机制测试与后续复用；生产列提交已改为 Schema-only。
    #[allow(dead_code)]
    Columns {
        before: Arc<Schema>,
        after: Arc<Schema>,
    },
}

/// 在两遍之间共享的轻量输出结构，不借用可变的 live 文档。
#[derive(Clone)]
struct FrozenSerialization {
    schema: Arc<Schema>,
    saved_headers: Vec<String>,
    sorted: Option<Arc<Perm>>,
}

impl FrozenSerialization {
    /// 创建一次序列化调用所需的短生命周期视图。
    fn view<'b>(&'b self, document: &'b BigFile) -> SerializationView<'b> {
        SerializationView {
            document,
            schema: self.schema.as_ref(),
            saved_headers: &self.saved_headers,
            sorted: self.sorted.as_deref(),
        }
    }
}

/// 一次物化的冻结输入、历史审批和可选进度回调。
pub(super) struct MaterializationInput<'a> {
    output: FrozenSerialization,
    approval: Approval,
    operation: MaterializationOperation,
    progress: Option<&'a mut dyn FnMut(u64, u64, u8, u64)>,
    fault: MaterializeFault,
    #[cfg(test)]
    workers: Option<usize>,
}

/// 测试用并行物化计数，按线程隔离以便并行测试互不干扰。
#[cfg(test)]
std::thread_local! {
    pub(super) static PARALLEL_MATERIALIZATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// 历史准备完成或需要固定点确认。
pub(super) enum HistoryPreparation {
    Ready(PreparedRamHistory),
    FixedPoint(FixedPointRequest),
}

/// 已准备的新正文及其全部配套提交状态。
pub(super) struct PreparedMaterialization {
    pub body: FinishedRam,
    pub stats: SerializationStats,
    pub index: PreparedIndexReuse,
    pub history: PreparedRamHistory,
    pub schema: Arc<Schema>,
    pub base_headers: Vec<String>,
    pub segments: Segments,
    pub order: RowOrder,
    pub next_history_generation: u64,
    pub next_index_epoch: u64,
    pub fixed_point: bool,
    pub content_key: RamContentKey,
}

/// 已准备好的历史重建或固定点清理。
pub(super) enum PreparedRamHistory {
    Rebuild(PreparedSaveHistory),
    FixedPointClear { budget_bytes: u64 },
}

impl PreparedRamHistory {
    /// 在新正文和段表已安装后提交历史，不执行分配或 I/O。
    fn commit_to(self, document: &mut BigFile) {
        match self {
            Self::Rebuild(prepared) => {
                document.seg.blocks = prepared.blocks;
                document.history.commit_rebuild(prepared.history);
            }
            Self::FixedPointClear { budget_bytes } => {
                document.history.clear();
                document.history.set_budget(budget_bytes);
            }
        }
    }
}

impl<'a> MaterializationInput<'a> {
    /// 冻结当前 Schema、表头和行序，供保存物化使用。
    pub(super) fn save_current(
        document: &BigFile,
        approval: Approval,
    ) -> Result<Self, String> {
        if approval.allow_fixed_point
            && approval.preflight_id != Some(document.history_generation)
        {
            return Err("固定点预检已过期，请重试本次保存".into());
        }
        let sorted = match &document.order {
            RowOrder::Identity => None,
            RowOrder::Perm(perm) => Some(Arc::clone(perm)),
        };
        Ok(Self {
            output: FrozenSerialization {
                schema: Arc::clone(&document.schema),
                saved_headers: document.view_headers(),
                sorted,
            },
            approval,
            operation: MaterializationOperation::SaveCurrent,
            progress: None,
            fault: MaterializeFault::None,
            #[cfg(test)]
            workers: None,
        })
    }

    /// 只读取 pending 候选 Schema；物化失败前不改变 live Schema 或 pending 预览。
    ///
    /// 生产路径已改为保存时统一物化（方案 A），此入口仅由物化机制测试使用，
    /// 保留以覆盖故障原子性与输出字节。
    #[allow(dead_code)]
    pub(super) fn columns(document: &BigFile, approval: Approval) -> Result<Self, String> {
        if document.pending_cols.is_empty() {
            return Err("没有待执行的列操作".into());
        }
        let after = document
            .pending_schema
            .clone()
            .ok_or("列操作预览已失效")?;
        let saved_headers = after
            .visible()
            .iter()
            .map(|id| {
                after
                    .resolve(*id)
                    .map(|column| column.header.clone())
                    .ok_or_else(|| "列操作候选 Schema 缺少可见列".to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            output: FrozenSerialization {
                schema: Arc::clone(&after),
                saved_headers,
                sorted: None,
            },
            approval,
            operation: MaterializationOperation::Columns {
                before: Arc::clone(&document.schema),
                after,
            },
            progress: None,
            fault: MaterializeFault::None,
            #[cfg(test)]
            workers: None,
        })
    }

    /// 接入现有的四元组进度回调。
    pub(super) fn with_progress(
        mut self,
        progress: &'a mut dyn FnMut(u64, u64, u8, u64),
    ) -> Self {
        self.progress = Some(progress);
        self
    }

    /// 设置一次受控故障，供原子性测试使用。
    #[cfg(test)]
    pub(super) fn with_fault(mut self, fault: MaterializeFault) -> Self {
        self.fault = fault;
        self
    }

    /// 覆盖并行度，供测试在 CI 上强制并行路径。
    #[cfg(test)]
    pub(super) fn with_test_workers(mut self, workers: usize) -> Self {
        self.workers = Some(workers);
        self
    }

    /// 返回本次物化使用的并行度。
    fn workers(&self) -> usize {
        #[cfg(test)]
        if let Some(workers) = self.workers {
            return workers;
        }
        materialize_workers()
    }

    /// 返回本次物化的逻辑数据行总数（排序时按排序长度）。
    fn total_rows(&self, document: &BigFile) -> u64 {
        self.output.sorted.as_ref().map_or_else(
            || document.seg.vrow_count(),
            |order| order.as_slice().len() as u64,
        )
    }

    /// 克隆冻结输出，避免第二遍借用第一遍的临时视图。
    fn output(&self) -> FrozenSerialization {
        self.output.clone()
    }

    /// 在旧正文仍可读取时完成历史转换、预算和固定点准备。
    fn prepare_history(
        &self,
        document: &BigFile,
        new_bytes: u64,
    ) -> Result<HistoryPreparation, String> {
        let budget_bytes = effective_undo_budget_bytes(new_bytes, true, document.undo_policy)?;
        match &self.operation {
            MaterializationOperation::SaveCurrent => {
                if self.output.sorted.is_some() {
                    if document.history.stats().undo_count + document.history.stats().redo_count > 0
                        && !self.approval.allow_fixed_point
                    {
                        return Ok(HistoryPreparation::FixedPoint(
                            document.save_fixed_point_request(
                                FixedPointKind::SortMaterialization,
                                "按当前排序保存并物化行序".into(),
                            ),
                        ));
                    }
                    return Ok(HistoryPreparation::Ready(
                        PreparedRamHistory::FixedPointClear { budget_bytes },
                    ));
                }
                match document.transform_history_for_materialization(&self.output.schema, None) {
                    Ok(transformed) => Ok(HistoryPreparation::Ready(
                        PreparedRamHistory::Rebuild(transformed.prepare_rebuild(
                            budget_bytes,
                            SaveRebuildPolicy::EvictOldest,
                        )?),
                    )),
                    Err(operation) if !self.approval.allow_fixed_point => {
                        Ok(HistoryPreparation::FixedPoint(
                            document.save_fixed_point_request(
                                FixedPointKind::SaveTransform,
                                operation,
                            ),
                        ))
                    }
                    Err(_) => Ok(HistoryPreparation::Ready(
                        PreparedRamHistory::FixedPointClear { budget_bytes },
                    )),
                }
            }
            MaterializationOperation::Columns { before, after } => {
                let operation = Op::Columns {
                    change: SchemaChange {
                        before: Arc::clone(before),
                        after: Arc::clone(after),
                    },
                };
                match document.preflight_history(
                    operation.clone(),
                    "执行列操作",
                    self.approval,
                )? {
                    HistoryDecision::Confirm(outcome) => Ok(HistoryPreparation::FixedPoint(
                        outcome.fixed_point.ok_or("列操作固定点预检缺少请求")?,
                    )),
                    HistoryDecision::FixedPoint(_) => Ok(HistoryPreparation::Ready(
                        PreparedRamHistory::FixedPointClear { budget_bytes },
                    )),
                    HistoryDecision::Normal(_) => {
                        match document.transform_history_for_materialization(
                            after,
                            Some(operation),
                        ) {
                            Ok(transformed) => Ok(HistoryPreparation::Ready(
                                PreparedRamHistory::Rebuild(transformed.prepare_rebuild(
                                    budget_bytes,
                                    SaveRebuildPolicy::EvictOldest,
                                )?),
                            )),
                            Err(operation) if !self.approval.allow_fixed_point => {
                                Ok(HistoryPreparation::FixedPoint(
                                    document.save_fixed_point_request(
                                        FixedPointKind::SaveTransform,
                                        operation,
                                    ),
                                ))
                            }
                            Err(_) => Ok(HistoryPreparation::Ready(
                                PreparedRamHistory::FixedPointClear { budget_bytes },
                            )),
                        }
                    }
                }
            }
        }
    }

    /// 把 CommitAt 转换为 RamRegion 的页启用故障偏移。
    fn enable_fault_at(&self) -> Option<usize> {
        match self.fault {
            MaterializeFault::CommitAt(offset) => usize::try_from(offset).ok(),
            _ => None,
        }
    }
}

/// 并行物化的最小总行数；低于该值顺序路径更快，也避免小文件开销。
pub(super) const PARALLEL_MIN_ROWS: u64 = 65_536;

/// 多块并行时每块的最少行数，保证块边界调度开销可忽略。
pub(super) const MIN_ROWS_PER_CHUNK: u64 = 8_192;

/// 内存物化工作线程上限；线程栈计入私有内存峰值。
pub(super) const MAX_MATERIALIZE_WORKERS: usize = 16;

/// 每工作线程分配的块数；适当超配以掩盖块间耗时差异。
const CHUNKS_PER_WORKER: usize = 4;

/// 返回本机内存物化并行度，上限受线程栈内存预算约束。
fn materialize_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_MATERIALIZE_WORKERS)
}

/// 把逻辑数据行 `[0, total)` 切分为首尾相接的非空行区间。
///
/// # Parameters
/// - `total`: 逻辑数据行总数。
/// - `workers`: 目标并行度；块数约为其四倍，但每块不少于
///   [`MIN_ROWS_PER_CHUNK`] 行。
///
/// # Returns
/// 从 0 起精确铺满 `total` 的连续区间；`total == 0` 时返回空表。
pub(super) fn chunk_spans(total: u64, workers: usize) -> Vec<RowSpan> {
    if total == 0 {
        return Vec::new();
    }
    let desired = workers.max(1).saturating_mul(CHUNKS_PER_WORKER);
    // 每块至少 MIN_ROWS_PER_CHUNK 行；小表退化为单块。
    let cap = usize::try_from(total / MIN_ROWS_PER_CHUNK)
        .unwrap_or(usize::MAX)
        .max(1);
    let count = desired.min(cap);
    let base = total / count as u64;
    let rem = total % count as u64;
    let mut spans = Vec::with_capacity(count);
    let mut start = 0u64;
    for index in 0..count {
        // 余数优先分给前面的块，块长最多相差 1 行。
        let len = base + u64::from((index as u64) < rem);
        spans.push(RowSpan {
            start,
            end: start + len,
        });
        start += len;
    }
    spans
}

/// 判断非排序输出是否存在“块边界落在物理行中间”的唯一布局。
///
/// 只有原始文件最后一条记录缺少末尾换行、且其后还有插入行时，顺序输出
/// 才会把插入行拼到同一条物理行；此时按行切块会切断物理行，统计不可加。
pub(super) fn has_line_split_boundary(document: &BigFile) -> bool {
    let bytes = document.bytes();
    if bytes.last() == Some(&b'\n') {
        return false;
    }
    let Some(last_record) = document.idx.n_records().checked_sub(1) else {
        return false;
    };
    let last_record = u32::try_from(last_record).unwrap_or(u32::MAX);
    for (index, segment) in document.seg.segs.iter().enumerate() {
        let SegKind::Src { start, len } = segment.kind else {
            continue;
        };
        // 只看终止于文件最后一条原始记录的段。
        if start != last_record.saturating_add(1).saturating_sub(len) {
            continue;
        }
        let later_inserts = document.seg.segs[index + 1..].iter().any(|segment| {
            matches!(
                segment.kind,
                SegKind::Ins { block }
                    if document
                        .seg
                        .blocks
                        .get(block as usize)
                        .and_then(Option::as_ref)
                        .is_some_and(|rows| !rows.rows.is_empty())
            )
        });
        if later_inserts {
            return true;
        }
    }
    false
}

/// 判断本次物化是否走并行两遍路径。
///
/// # Parameters
/// - `document`: 当前文档（只读取段表与物理索引）。
/// - `total`: 逻辑数据行总数（排序时为排序长度）。
/// - `workers`: 可用并行度。
/// - `fault`: 测试注入的受控故障；任何故障都回退顺序路径保持原语义。
/// - `sorted`: 是否按排序顺序输出；排序分支会给缺换行的记录补分隔符。
pub(super) fn should_parallelize(
    document: &BigFile,
    total: u64,
    workers: usize,
    fault: MaterializeFault,
    sorted: bool,
) -> bool {
    workers >= 2
        && total >= PARALLEL_MIN_ROWS
        && fault == MaterializeFault::None
        && (sorted || !has_line_split_boundary(document))
}

/// 并行执行计数遍：每个行区间独立计数，工作线程只写自己的进度槽。
///
/// 进度回调始终由调用线程在轮询时触发，因此回调无需 `Send`；
/// 所有工作线程汇合后还会无条件回调一次 `done == total` 的终态。
///
/// # Parameters
/// - `view`: 冻结的序列化视图。
/// - `spans`: 首尾相接、精确覆盖整篇的行区间。
/// - `workers`: 实际启动的工作线程数，按区间数截断。
/// - `on_progress`: 轮询期间与结束时由调用线程收到的聚合进度。
///
/// # Returns
/// 按块序排列的各块统计，以及合并后的整篇统计。
///
/// # Errors
/// 任一区间计数失败、合并溢出或行数不一致时返回首个错误。
pub(super) fn count_spans_parallel(
    view: SerializationView<'_>,
    spans: &[RowSpan],
    workers: usize,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(Vec<SerializationStats>, SerializationStats), String> {
    if spans.is_empty() {
        return Err("并行计数至少需要一个行区间".into());
    }
    let context = SerializationContext::new(view);
    let total = context.total_rows();
    // 每块一个独占原子槽，工作线程之间没有共享计数器争用。
    let done: Vec<AtomicU64> = spans.iter().map(|_| AtomicU64::new(0)).collect();
    let results: Mutex<Vec<Option<SerializationStats>>> = Mutex::new(vec![None; spans.len()]);
    let error = Mutex::new(None::<String>);
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let count = workers.min(spans.len()).max(1);
    let record = |text: String| {
        let mut slot = error.lock().unwrap();
        if slot.is_none() {
            *slot = Some(text);
        }
    };
    std::thread::scope(|scope| {
        for _ in 0..count {
            let spawned = std::thread::Builder::new()
                // 线程栈同样计入私有内存峰值，64 MiB 门槛余量有限。
                .stack_size(512 * 1024)
                .spawn_scoped(scope, || {
                    loop {
                        if error.lock().unwrap().is_some() {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= spans.len() {
                            break;
                        }
                        let span = spans[index];
                        let mut sink = CountingSink::new();
                        let result = context.serialize_row_range(
                            &mut sink,
                            span,
                            span.start == 0,
                            |rows| {
                                // 只写本块槽位，读侧在调用线程求和。
                                done[index].store(rows, Ordering::Relaxed);
                            },
                        );
                        match result {
                            Ok(stats) => {
                                results.lock().unwrap()[index] = Some(stats);
                                finished.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(text) => {
                                record(text);
                                break;
                            }
                        }
                    }
                });
            if spawned.is_err() {
                record("创建并行计数线程失败".into());
            }
        }
        // 调用线程轮询聚合进度；回调因此不跨线程，也天然单调。
        while finished.load(Ordering::Relaxed) < spans.len()
            && error.lock().unwrap().is_none()
        {
            let current = done.iter().map(|slot| slot.load(Ordering::Relaxed)).sum();
            on_progress(current, total);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    if let Some(text) = error.into_inner().unwrap() {
        return Err(text);
    }
    let slots = results.into_inner().unwrap();
    let mut combined: Option<SerializationStats> = None;
    let mut ordered = Vec::with_capacity(slots.len());
    for (index, slot) in slots.into_iter().enumerate() {
        let slot = slot.ok_or_else(|| format!("并行计数缺少第 {index} 块统计"))?;
        combined = Some(match combined {
            None => slot,
            Some(previous) => previous.combine(slot)?,
        });
        ordered.push(slot);
    }
    let combined = combined.ok_or("并行计数没有产生统计")?;
    if combined.rows != total {
        return Err(format!(
            "并行计数行数不一致：预期 {total}，实际 {}",
            combined.rows
        ));
    }
    // 阶段边界无条件补终态，保证前端能显示 100%。
    on_progress(total, total);
    Ok((ordered, combined))
}

/// 并行执行生成遍：各工作线程把对应行区间直接写进新正文的独占切片。
///
/// 区间偏移由第一遍计数精确得出，因此不需要任何临时块缓冲，
/// 新增私有内存恰好是一份新正文。
///
/// # Parameters
/// - `view`: 冻结的序列化视图。
/// - `spans`: 与计数遍完全相同的行区间。
/// - `counted`: 计数遍得到的逐块统计；用于计算偏移并做结构一致性硬校验。
/// - `region`: 已按总字节数预留的新正文区域。
/// - `workers`: 实际启动的工作线程数。
/// - `on_progress`: 轮询期间与结束时由调用线程收到的聚合进度。
///
/// # Returns
/// 完整写入的只读新正文。
///
/// # Errors
/// 块数不一致、偏移溢出、区间写入失败或任一逐块统计与计数遍不一致时返回错误。
pub(super) fn generate_spans_parallel(
    view: SerializationView<'_>,
    spans: &[RowSpan],
    counted: &[SerializationStats],
    region: RamRegion,
    workers: usize,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<FinishedRam, String> {
    if spans.is_empty() {
        return Err("并行生成至少需要一个行区间".into());
    }
    if counted.len() != spans.len() {
        return Err(format!(
            "并行生成块数不一致：区间 {}，统计 {}",
            spans.len(),
            counted.len()
        ));
    }
    let context = SerializationContext::new(view);
    let total = context.total_rows();
    // 前缀和得到每块在新正文中的精确字节偏移。
    let mut ranges = Vec::with_capacity(spans.len());
    let mut offset = 0usize;
    for stats in counted {
        let len = usize::try_from(stats.bytes).map_err(|_| "正文块长度超出本机地址空间")?;
        ranges.push(RangeSpan { offset, len });
        offset = offset.checked_add(len).ok_or("正文块偏移溢出")?;
    }
    if offset != region.len() {
        return Err(format!(
            "并行生成长度不一致：预期 {}，统计合计 {offset}",
            region.len()
        ));
    }
    // 每块一个独占进度槽与统计槽；工作线程之间不共享可变状态。
    let done: Vec<AtomicU64> = spans.iter().map(|_| AtomicU64::new(0)).collect();
    let written: Mutex<Vec<Option<SerializationStats>>> = Mutex::new(vec![None; spans.len()]);
    let mut region = region;
    region.for_each_range_mut(
        &ranges,
        workers,
        |_| {},
        |index, out| {
            let mut sink = SliceSink::new(out);
            let span = spans[index];
            let stats = context.serialize_row_range(
                &mut sink,
                span,
                span.start == 0,
                |rows| {
                    done[index].store(rows, Ordering::Relaxed);
                },
            )?;
            // 切片必须被精确填满，否则偏移与前缀和不符。
            sink.finish()?;
            written.lock().unwrap()[index] = Some(stats);
            Ok(())
        },
        || {
            let current = done.iter().map(|slot| slot.load(Ordering::Relaxed)).sum();
            on_progress(current, total);
        },
    )?;
    let slots = written.into_inner().unwrap();
    let mut combined: Option<SerializationStats> = None;
    for (index, slot) in slots.into_iter().enumerate() {
        let stats = slot.ok_or_else(|| format!("并行生成缺少第 {index} 块统计"))?;
        // 两遍结构一致性硬校验：任何字段不同都说明分块边界或序列化有误。
        if stats != counted[index] {
            return Err(format!(
                "并行生成统计不一致：第 {index} 块 {:?} != {:?}",
                counted[index], stats
            ));
        }
        combined = Some(match combined {
            None => stats,
            Some(previous) => previous.combine(stats)?,
        });
    }
    let combined = combined.ok_or("并行生成没有产生统计")?;
    if combined.rows != total {
        return Err(format!(
            "并行生成行数不一致：预期 {total}，实际 {}",
            combined.rows
        ));
    }
    // 阶段边界无条件补终态，保证前端能显示 100%。
    on_progress(total, total);
    region.finish()
}

/// 只保存新索引所需容量和格式，不借用旧索引或旧正文。
#[derive(Clone, Copy, Debug)]
pub(super) struct PreparedIndexReuse {
    line_capacity: usize,
    record_capacity: usize,
    format: DocumentFormat,
}

impl RawIndex {
    /// 在旧索引仍可读时预留新索引容量；不会清空或重排旧向量。
    fn prepare_reuse(
        &mut self,
        stats: SerializationStats,
    ) -> Result<PreparedIndexReuse, String> {
        // rec_lines 保存物理行号，最大行号必须能由 u32 表示。
        u32::try_from(stats.line_capacity.saturating_sub(1))
            .map_err(|_| "物理行号超过 u32")?;
        u32::try_from(stats.record_capacity).map_err(|_| "索引记录数超过 u32")?;
        if self.line_off.capacity() < stats.line_capacity {
            let additional = stats.line_capacity.saturating_sub(self.line_off.len());
            self.line_off
                .try_reserve(additional)
                .map_err(|_| "新索引物理行容量不足")?;
        }
        if self.rec_lines.capacity() < stats.record_capacity {
            let additional = stats.record_capacity.saturating_sub(self.rec_lines.len());
            self.rec_lines
                .try_reserve(additional)
                .map_err(|_| "新索引记录容量不足")?;
        }
        Ok(PreparedIndexReuse {
            line_capacity: stats.line_capacity,
            record_capacity: stats.record_capacity,
            format: match stats.rows {
                // 格式由 commit 调用方核对；这里不占位伪造格式。
                _ => DocumentFormat::Csv,
            },
        })
    }

    /// 复用已准备好的向量容量扫描新正文；不在 RAM 提交段分配或返回错误。
    fn scan_reusing_capacity(
        &mut self,
        bytes: &[u8],
        format: DocumentFormat,
        prepared: PreparedIndexReuse,
    ) {
        assert_eq!(format, prepared.format, "物化索引格式在提交期间改变");
        assert!(self.line_off.capacity() >= prepared.line_capacity);
        assert!(self.rec_lines.capacity() >= prepared.record_capacity);
        self.line_off.clear();
        self.rec_lines.clear();
        self.line_off.push(0);
        self.line_off.extend(
            memchr_iter(b'\n', bytes).map(|position| (position + 1) as u64),
        );
        assert_eq!(self.line_off.len(), prepared.line_capacity);

        match format {
            DocumentFormat::Csv => {
                let mut quote = QuoteMachine::new(false);
                for line in 0..self.line_off.len() {
                    let start = self.line_off[line] as usize;
                    let end = if line + 1 < self.line_off.len() {
                        self.line_off[line + 1] as usize
                    } else {
                        bytes.len()
                    };
                    let started_open = quote.in_quotes;
                    quote.feed(&bytes[start..end]);
                    if !started_open && !line_is_blank(bytes, start, end) {
                        self.rec_lines
                            .push(u32::try_from(line).expect("物理行号必须在预留阶段校验"));
                    }
                }
            }
            DocumentFormat::TextLines => {
                for line in 0..self.line_off.len() {
                    if self.line_off[line] < bytes.len() as u64 {
                        self.rec_lines
                            .push(u32::try_from(line).expect("物理行号必须在预留阶段校验"));
                    }
                }
            }
        }
        assert_eq!(self.rec_lines.len(), prepared.record_capacity);
    }
}

/// 执行两遍序列化并准备新正文；任何错误都不接管旧 RAM。
pub(super) fn prepare_materialization<O>(
    document: &mut BigFile,
    mut input: MaterializationInput<'_>,
    observer: &O,
) -> Result<PrepareMaterialization, String>
where
    O: Fn(SaveRamEvent) + Sync,
{
    observer(SaveRamEvent::Phase(SaveRamPhase::CountBegin));
    let workers = input.workers();
    let total_rows = input.total_rows(document);
    // 本函数只服务内存模式；非内存保存不走这里，顺序行为完全不变。
    let parallel = should_parallelize(
        document,
        total_rows,
        workers,
        input.fault,
        input.output.sorted.is_some(),
    );
    let spans = if parallel {
        #[cfg(test)]
        PARALLEL_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
        Some(chunk_spans(total_rows, workers))
    } else {
        None
    };
    let (counted, per_chunk) = match spans.as_deref() {
        Some(spans) => {
            let output = input.output();
            let mut progress = input.progress.as_deref_mut();
            let mut last_emit: Option<Instant> = None;
            let (per_chunk, combined) = count_spans_parallel(
                output.view(document),
                spans,
                workers,
                |done, total| {
                    // 中间进度按时间门限聚合；终态由辅助函数在汇合后无条件回调。
                    if done < total
                        && !super::progress_gate(&mut last_emit, super::PROGRESS_MIN_GAP)
                    {
                        return;
                    }
                    if let Some(progress) = progress.as_deref_mut() {
                        progress(done, total, super::PHASE_STREAM, done);
                    }
                },
            )?;
            (combined, Some(per_chunk))
        }
        None => {
            let output = input.output();
            let mut sink = CountingSink::new();
            let mut progress = input.progress.as_deref_mut();
            // 与磁盘保存路径一致：逐行回调先按 16 行掩码再按时间门限节流，
            // 否则数百万行会把同等数量的 IPC 事件灌给 WebView，拖死主线程与渲染进程。
            let mut last_emit: Option<Instant> = None;
            let counted = serialize_document(output.view(document), &mut sink, |done, total| {
                if done & 0xF == 0
                    && super::progress_gate(&mut last_emit, super::PROGRESS_MIN_GAP)
                {
                    if let Some(progress) = progress.as_deref_mut() {
                        progress(done, total, super::PHASE_STREAM, done);
                    }
                }
            })?;
            // 阶段边界无条件补一条终态，保证前端能显示 100%。
            if let Some(progress) = progress.as_deref_mut() {
                progress(
                    counted.rows,
                    counted.rows,
                    super::PHASE_STREAM,
                    counted.rows,
                );
            }
            (counted, None)
        }
    };
    if matches!(input.fault, MaterializeFault::CountAfter(limit) if counted.rows >= limit) {
        return Err(format!("注入第一遍计数故障：{} 条记录", counted.rows));
    }
    observer(SaveRamEvent::Phase(SaveRamPhase::CountFinished {
        new_bytes: counted.bytes,
        line_capacity: counted.line_capacity,
        record_capacity: counted.record_capacity,
    }));
    let history = match input.prepare_history(document, counted.bytes)? {
        HistoryPreparation::Ready(history) => history,
        HistoryPreparation::FixedPoint(request) => {
            return Ok(PrepareMaterialization::FixedPoint(request));
        }
    };
    observer(SaveRamEvent::Phase(SaveRamPhase::HistoryPrepared));
    let mut index = document.idx.prepare_reuse(counted)?;
    index.format = document.format;
    observer(SaveRamEvent::Phase(SaveRamPhase::IndexPrepared {
        line_capacity: counted.line_capacity,
        record_capacity: counted.record_capacity,
    }));

    // 所有可失败的算术、代际和分配型状态都先于第二份 RAM 与 NewRamReady。
    let new_len = usize::try_from(counted.bytes).map_err(|_| "正文超过本机地址空间")?;
    let n_data = u32::try_from(counted.rows).map_err(|_| "正文记录数超过 u32")?;
    let next_history_generation = document.next_history_generation()?;
    let next_index_epoch = document
        .index_epoch
        .checked_add(1)
        .ok_or("索引代际溢出")?;
    let segments = Segments::new(u32::from(!document.headerless), n_data);
    let fixed_point = matches!(&history, PreparedRamHistory::FixedPointClear { .. });
    let output = input.output();
    // 序列化候选 Schema 可能含稳定隐藏列；提交后的基线 Schema 必须只含新正文可见 Source 列。
    let committed_schema = match &history {
        PreparedRamHistory::Rebuild(prepared) => Arc::clone(&prepared.schema),
        PreparedRamHistory::FixedPointClear { .. } => {
            Arc::new(Schema::from_headers(output.saved_headers.clone())?)
        }
    };
    let content_key = RamContentKey {
        history_generation: next_history_generation,
        schema: Arc::clone(&committed_schema),
        order: RowOrder::Identity,
        output_encoding: document.output_encoding,
        format: document.format,
        headerless: document.headerless,
        has_bom: document.has_bom,
    };
    observer(SaveRamEvent::Phase(SaveRamPhase::NewReserveBegin {
        new_bytes: counted.bytes,
    }));
    if input.fault == MaterializeFault::Reserve {
        return Err("注入第二份 RAM 预留失败".into());
    }
    if matches!(input.fault, MaterializeFault::CommitAt(_)) && input.enable_fault_at().is_none() {
        return Err("第二遍提交故障偏移超出本机地址空间".into());
    }
    let region = RamRegion::reserve_with_enable_fault(new_len, input.enable_fault_at())?;
    let body = match (spans.as_deref(), per_chunk.as_deref()) {
        (Some(spans), Some(per_chunk)) => {
            let output = input.output();
            let mut progress = input.progress.as_deref_mut();
            let mut last_emit: Option<Instant> = None;
            generate_spans_parallel(
                output.view(document),
                spans,
                per_chunk,
                region,
                workers,
                |done, total| {
                    if done < total
                        && !super::progress_gate(&mut last_emit, super::PROGRESS_MIN_GAP)
                    {
                        return;
                    }
                    if let Some(progress) = progress.as_deref_mut() {
                        progress(done, total, super::PHASE_FLUSH, done);
                    }
                },
            )?
        }
        _ => {
            let mut sink = RamRegionSink::new(region);
            let written = {
                let mut progress = input.progress.as_deref_mut();
                // 第二遍同样节流，避免逐行事件风暴。
                let mut last_emit: Option<Instant> = None;
                let written =
                    serialize_document(output.view(document), &mut sink, |done, total| {
                        if done & 0xF == 0
                            && super::progress_gate(&mut last_emit, super::PROGRESS_MIN_GAP)
                        {
                            if let Some(progress) = progress.as_deref_mut() {
                                progress(done, total, super::PHASE_FLUSH, done);
                            }
                        }
                    })?;
                // 阶段边界无条件补一条终态，保证前端能显示 100%。
                if let Some(progress) = progress.as_deref_mut() {
                    progress(written.rows, written.rows, super::PHASE_FLUSH, written.rows);
                }
                written
            };
            if written != counted {
                return Err(format!("两遍输出结构不一致：{counted:?} != {written:?}"));
            }
            sink.finish()?
        }
    };
    observer(SaveRamEvent::Phase(SaveRamPhase::NewRamReady {
        new_bytes: counted.bytes,
    }));
    Ok(PrepareMaterialization::Ready(PreparedMaterialization {
        body,
        stats: counted,
        index,
        history,
        schema: committed_schema,
        base_headers: output.saved_headers,
        segments,
        order: RowOrder::Identity,
        next_history_generation,
        next_index_epoch,
        fixed_point,
        content_key,
    }))
}

/// 只移动已准备好的状态；新正文安装后一次释放旧正文，不触碰磁盘保存点。
pub(super) fn commit_ram<O>(
    document: &mut BigFile,
    prepared: PreparedMaterialization,
    observer: &O,
) where
    O: Fn(SaveRamEvent) + Sync,
{
    let PreparedMaterialization {
        body,
        stats,
        index: index_plan,
        history,
        schema,
        base_headers,
        segments,
        order,
        next_history_generation,
        next_index_epoch,
        fixed_point,
        content_key,
    } = prepared;
    let old_bytes = document.backing.bytes().len() as u64;
    let had_history =
        document.history.stats().undo_count + document.history.stats().redo_count > 0;
    let mut index = std::mem::take(&mut document.idx);
    index.scan_reusing_capacity(body.as_slice(), document.format, index_plan);
    let previous = std::mem::replace(
        &mut document.backing,
        Backing::Ram(RamBuffer::Region(body)),
    );
    drop(previous);
    observer(SaveRamEvent::Phase(SaveRamPhase::OldRamReleased {
        old_bytes,
    }));
    document.idx = index;
    document.partial = None;
    document.total_bytes = stats.bytes;
    document.seg = segments;
    document.overlay.clear();
    document.stable_hits = None;
    document.display_hits = None;
    document.last_spec = None;
    document.schema = schema;
    document.base_ncols = base_headers.len();
    document.base_headers = base_headers;
    document.order = order;
    document.fixed_point |= fixed_point;
    document.history_generation = next_history_generation;
    document.index_epoch = next_index_epoch;
    document.backing_encoding = document.output_encoding;
    history.commit_to(document);
    if had_history && document.history.stats().undo_count + document.history.stats().redo_count == 0 {
        // 新正文预算自动淘汰全部旧历史时，同样建立固定点。
        document.fixed_point = true;
    }
    document.disk_body_dirty = true;
    document.ram_content_key = Some(content_key);
}

const DISK_COPY_CHUNK: usize = 1024 * 1024;

/// 把已经提交的连续 RAM 有界写入临时文件，关闭并同步后返回其文件状态。
fn write_completed_ram<F, O>(
    ram: &FinishedRam,
    tmp: &Path,
    observer: &O,
    mut progress: F,
) -> Result<FileStamp, String>
where
    F: FnMut(u64, u64),
    O: Fn(SaveRamEvent) + Sync,
{
    #[cfg(test)]
    if super::memory_save_tests::disk_write_fault() == DiskWriteFault::Create {
        return Err("注入创建临时文件失败".into());
    }
    let out = fs::File::create(tmp).map_err(|error| format!("创建临时文件失败：{error}"))?;
    let mut writer = BufWriter::with_capacity(DISK_COPY_CHUNK, out);
    observer(SaveRamEvent::Phase(SaveRamPhase::DiskWriteBegin {
        new_bytes: ram.len() as u64,
    }));
    let mut copied = 0u64;
    for chunk in ram.as_slice().chunks(DISK_COPY_CHUNK) {
        writer
            .write_all(chunk)
            .map_err(|error| format!("写入临时文件失败：{error}"))?;
        copied = copied
            .checked_add(chunk.len() as u64)
            .ok_or("写盘长度溢出")?;
        #[cfg(test)]
        if let DiskWriteFault::WriteAfter(limit) = super::memory_save_tests::disk_write_fault() {
            if copied >= limit {
                return Err("注入写入临时文件失败".into());
            }
        }
        progress(copied, ram.len() as u64);
    }
    #[cfg(test)]
    if super::memory_save_tests::disk_write_fault() == DiskWriteFault::Flush {
        return Err("注入刷新临时文件失败".into());
    }
    writer
        .flush()
        .map_err(|error| format!("刷新临时文件失败：{error}"))?;
    let file = writer
        .into_inner()
        .map_err(|error| format!("关闭写器失败：{}", error.error()))?;
    #[cfg(test)]
    if super::memory_save_tests::disk_write_fault() == DiskWriteFault::Sync {
        return Err("注入同步临时文件失败".into());
    }
    file.sync_all()
        .map_err(|error| format!("同步临时文件失败：{error}"))?;
    drop(file);
    let stamp = FileStamp::read(tmp)?;
    observer(SaveRamEvent::Phase(SaveRamPhase::DiskWriteFinished {
        new_bytes: ram.len() as u64,
    }));
    Ok(stamp)
}

/// 只重试临时文件换名；失败绝不重映射或替换当前 RAM 正文。
fn replace_materialized_with_retry(tmp: &Path, dest: &Path) -> Result<(), String> {
    let mut last = None;
    for _ in 0..5 {
        match fs::rename(tmp, dest) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
    Err(format!(
        "写入目标文件失败：{}\n新文件已完整写在临时文件里（数据未丢失）：{}\n\
         可关闭占用目标文件的程序后重试保存，或手动把该临时文件改名为目标文件。",
        last.map(|error| error.to_string()).unwrap_or_default(),
        tmp.display(),
    ))
}

impl BigFile {
    /// 用当前逻辑状态构造 RAM 内容键；revision 和磁盘保存点不参与。
    pub(super) fn current_ram_content_key(&self) -> RamContentKey {
        RamContentKey {
            history_generation: self.history_generation,
            schema: Arc::clone(&self.schema),
            order: self.order.clone(),
            output_encoding: self.output_encoding,
            format: self.format,
            headerless: self.headerless,
            has_bom: self.has_bom,
        }
    }

    /// 只有持久内存模式下的最终连续 RAM 且键完全相等时才允许直接复用。
    pub(super) fn persistent_ram_is_current(&self) -> bool {
        self.memory_mode
            && matches!(&self.backing, Backing::Ram(RamBuffer::Region(_)))
            && self.ram_content_key.as_ref() == Some(&self.current_ram_content_key())
    }

    /// 生产保存与测试共用同一个双遍物化入口。
    pub(super) fn prepare_materialization<O>(
        &mut self,
        input: MaterializationInput<'_>,
        observer: &O,
    ) -> Result<PrepareMaterialization, String>
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        prepare_materialization(self, input, observer)
    }

    /// 生产保存与测试共用同一个无 I/O RAM 提交入口。
    pub(super) fn commit_ram<O>(&mut self, prepared: PreparedMaterialization, observer: &O)
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        commit_ram(self, prepared, observer);
    }

    /// 换名成功后的唯一磁盘保存点提交，不执行 I/O 或可失败转换。
    fn commit_disk_success(&mut self, dest: &Path, switch_path: bool, stamp: FileStamp) {
        if switch_path {
            self.path = dest.to_path_buf();
        }
        self.backing_path = self.path.clone();
        self.base_size = stamp.len;
        self.base_mtime = stamp.modified;
        self.revision = 0;
        self.savepoint_detached = false;
        self.disk_body_dirty = false;
    }

    /// 从当前权威 RAM 写盘；RAM 提交后的所有错误都保留明确前缀。
    pub(super) fn commit_persistent_ram_to_disk<F, O>(
        &mut self,
        dest: &Path,
        switch_path: bool,
        mut progress: F,
        observer: O,
    ) -> Result<SaveOutcome, String>
    where
        F: FnMut(u64, u64, u8, u64),
        O: Fn(SaveRamEvent) + Sync,
    {
        let result = (|| {
            let tmp = super::tmp_path_for(dest);
            let rows = self.seg.vrow_count();
            let ram = match &self.backing {
                Backing::Ram(RamBuffer::Region(ram)) => ram,
                _ => return Err("持久内存正文不是连续 RAM 区域".into()),
            };
            let new_bytes = ram.len() as u64;
            let stamp = write_completed_ram(ram, &tmp, &observer, |done, total| {
                progress(done, total, super::PHASE_REINDEX, rows);
            })?;
            if !switch_path && self.stale() {
                return Err(format!(
                    "EXTERNAL_CHANGED: 写盘后发现文件发生变化\n临时文件保留在：{}",
                    tmp.display(),
                ));
            }
            replace_materialized_with_retry(&tmp, dest)?;
            self.commit_disk_success(dest, switch_path, stamp);
            observer(SaveRamEvent::Phase(SaveRamPhase::DiskCommitted {
                new_bytes,
            }));
            progress(rows, rows, super::PHASE_COMMIT, rows);
            Ok(SaveOutcome {
                saved: true,
                fixed_point: None,
                cancelled: false,
            })
        })();
        result.map_err(|error: String| format!("{}{error}", super::RAM_COMMITTED_ERROR))
    }
}

#[cfg(test)]
impl BigFile {
    /// 返回非空后备地址，用于区分旧 RAM 与新 RAM 的身份。
    pub(super) fn backing_identity(&self) -> usize {
        assert!(!self.backing.bytes().is_empty(), "空文档不能用地址证明身份");
        self.backing.bytes().as_ptr() as usize
    }

    /// 测试入口与生产准备函数保持同一实现。
    pub(super) fn prepare_materialization_for_test<O>(
        &mut self,
        input: MaterializationInput<'_>,
        observer: O,
    ) -> Result<PrepareMaterialization, String>
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        prepare_materialization(self, input, &observer)
    }
}
