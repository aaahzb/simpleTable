//! 文档正文的唯一序列化入口，以及计数、I/O 和连续 RAM 写出器。

use super::{
    encode_document_record, BigFile, DocumentFormat, QuoteMachine, ResolvedRow, SegKind,
};
use crate::order::Perm;
use crate::ram_region::{FinishedRam, RamRegion};
use crate::schema::{ColumnStorage, Schema};
use memchr::memchr_iter;

/// 接收完整正文字节片段的写出器。
pub(super) trait DocumentSink {
    /// 写入完整片段；失败时由具体写出器保留底层错误。
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// 一次物化期间冻结的文档、Schema、表头和行序视图。
#[derive(Clone, Copy)]
pub(super) struct SerializationView<'a> {
    pub document: &'a BigFile,
    pub schema: &'a Schema,
    pub saved_headers: &'a [String],
    pub sorted: Option<&'a Perm>,
}

/// 从实际输出流统计的正文长度和索引容量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SerializationStats {
    pub bytes: u64,
    pub line_capacity: usize,
    pub record_capacity: usize,
    pub rows: u64,
}

impl SerializationStats {
    /// 把后一个行区间的统计并入前一个区间。
    ///
    /// `line_capacity` 每个新区间都从 1 开始（`line_off[0] = 0`），
    /// 且所有并行边界都落在物理行起点，因此合并时减 1；其余字段直接相加。
    ///
    /// # Parameters
    /// - `later`: 紧接在当前区间之后、非空行区间的统计。
    ///
    /// # Returns
    /// 合并后的整篇统计。
    ///
    /// # Errors
    /// 任一字段溢出时返回错误。
    pub(super) fn combine(self, later: Self) -> Result<Self, String> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_add(later.bytes)
                .ok_or("正文长度溢出")?,
            line_capacity: self
                .line_capacity
                .checked_add(later.line_capacity)
                .and_then(|value| value.checked_sub(1))
                .ok_or("物理行容量溢出")?,
            record_capacity: self
                .record_capacity
                .checked_add(later.record_capacity)
                .ok_or("索引记录数溢出")?,
            rows: self.rows.checked_add(later.rows).ok_or("正文行数溢出")?,
        })
    }
}

/// 一次物化中的逻辑数据行区间，`end` 不含。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowSpan {
    pub start: u64,
    pub end: u64,
}

impl RowSpan {
    /// 返回区间包含的逻辑数据行数。
    pub(super) fn len(self) -> u64 {
        self.end - self.start
    }
}

/// 只计算字节数，不保留正文副本的写出器。
pub(super) struct CountingSink {
    bytes: usize,
}

impl CountingSink {
    /// 创建零长度计数器。
    pub(super) fn new() -> Self {
        Self { bytes: 0 }
    }

    /// 返回已成功接收的正文字节数。
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

impl DocumentSink for CountingSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or("正文长度溢出")?;
        Ok(())
    }
}

/// 把正文写入现有阻塞 I/O 写器。
pub(super) struct IoSink<'a, W: std::io::Write> {
    pub inner: &'a mut W,
}

impl<W: std::io::Write> DocumentSink for IoSink<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        std::io::Write::write_all(self.inner, bytes).map_err(|error| error.to_string())
    }
}

/// 把第二遍正文顺序写入准确预留的连续匿名 RAM。
pub(super) struct RamRegionSink {
    region: Option<RamRegion>,
    written: usize,
}

impl RamRegionSink {
    /// 包装尚未开始写入的预留区域。
    pub(super) fn new(region: RamRegion) -> Self {
        Self {
            region: Some(region),
            written: 0,
        }
    }

    /// 核对第二遍实际长度，并生成只读完成态。
    pub(super) fn finish(mut self) -> Result<FinishedRam, String> {
        let region = self.region.take().ok_or("RAM 写出器已完成")?;
        if self.written != region.len() {
            return Err(format!(
                "第二遍正文长度不一致：预期 {}，实际 {}",
                region.len(),
                self.written,
            ));
        }
        region.finish()
    }
}

impl DocumentSink for RamRegionSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.written = self.written.checked_add(bytes.len()).ok_or("正文长度溢出")?;
        self.region
            .as_mut()
            .ok_or("RAM 写出器已完成")?
            .append(bytes)
    }
}

/// 把正文直接写入新正文中预计算大小的独占切片。
pub(super) struct SliceSink<'a> {
    out: &'a mut [u8],
    written: usize,
}

impl<'a> SliceSink<'a> {
    /// 包装一个必须被精确填满的目标切片。
    pub(super) fn new(out: &'a mut [u8]) -> Self {
        Self { out, written: 0 }
    }

    /// 校验切片已按字节精确填满，并返回写入长度。
    ///
    /// # Returns
    /// 实际写入的字节数，等于切片长度。
    ///
    /// # Errors
    /// 未填满切片时返回错误。
    pub(super) fn finish(self) -> Result<usize, String> {
        if self.written != self.out.len() {
            return Err(format!(
                "切片正文长度不一致：预期 {}，实际 {}",
                self.out.len(),
                self.written,
            ));
        }
        Ok(self.written)
    }
}

impl DocumentSink for SliceSink<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        let end = self
            .written
            .checked_add(bytes.len())
            .ok_or("正文长度溢出")?;
        let target = self
            .out
            .get_mut(self.written..end)
            .ok_or("切片正文写入溢出")?;
        target.copy_from_slice(bytes);
        self.written = end;
        Ok(())
    }
}

/// 流式复现 RawIndex 的物理行与记录计数规则，不保存偏移向量。
struct IndexCounter {
    format: DocumentFormat,
    bytes: u64,
    line_capacity: usize,
    record_capacity: usize,
    quote: QuoteMachine,
    line_started_open: bool,
    line_blank: bool,
    line_has_bytes: bool,
}

impl IndexCounter {
    /// 从输出文件起点开始计数；line_off 的首元素始终为 0。
    fn new(format: DocumentFormat) -> Self {
        Self {
            format,
            bytes: 0,
            line_capacity: 1,
            record_capacity: 0,
            quote: QuoteMachine::new(false),
            line_started_open: false,
            line_blank: true,
            line_has_bytes: false,
        }
    }

    /// 观察一段成功序列化的字节，可跨片段延续当前物理行和引号状态。
    fn observe(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or("正文长度溢出")?;
        let mut start = 0usize;
        for newline in memchr_iter(b'\n', bytes) {
            self.observe_complete_line(&bytes[start..=newline]);
            self.line_capacity = self
                .line_capacity
                .checked_add(1)
                .ok_or("物理行容量溢出")?;
            start = newline + 1;
        }
        if start < bytes.len() {
            self.observe_line_tail(&bytes[start..]);
        }
        Ok(())
    }

    /// 结束一条带换行的物理行，并按格式决定是否产生记录。
    fn observe_complete_line(&mut self, line: &[u8]) {
        self.line_has_bytes = true;
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        self.line_blank &= content.iter().all(|byte| *byte == b'\r');
        self.quote.feed(line);
        self.count_current_record();
        self.line_started_open = self.quote.in_quotes;
        self.line_blank = true;
        self.line_has_bytes = false;
    }

    /// 延续尚未遇到换行的物理行。
    fn observe_line_tail(&mut self, tail: &[u8]) {
        self.line_has_bytes = true;
        self.line_blank &= tail.iter().all(|byte| *byte == b'\r');
        self.quote.feed(tail);
    }

    /// 按当前行起始时的引号状态复现顺序索引记录判定。
    fn count_current_record(&mut self) {
        let is_record = match self.format {
            DocumentFormat::Csv => !self.line_started_open && !self.line_blank,
            DocumentFormat::TextLines => true,
        };
        if is_record {
            self.record_capacity += 1;
        }
    }

    /// 处理 EOF 前不带换行的最后一条物理行并返回容量统计。
    fn finish(mut self, rows: u64) -> Result<SerializationStats, String> {
        if self.line_has_bytes {
            self.count_current_record();
        }
        let bytes = usize::try_from(self.bytes).map_err(|_| "正文长度超出本机地址范围")?;
        if bytes as u64 != self.bytes {
            return Err("正文长度超出本机地址范围".into());
        }
        Ok(SerializationStats {
            bytes: self.bytes,
            line_capacity: self.line_capacity,
            record_capacity: self.record_capacity,
            rows,
        })
    }
}

/// 在写出前观察同一份字节，保证计数与任何实际目标完全一致。
struct ObservedSink<'a, S: DocumentSink> {
    inner: &'a mut S,
    counter: IndexCounter,
}

impl<S: DocumentSink> DocumentSink for ObservedSink<'_, S> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.counter.observe(bytes)?;
        self.inner.write_all(bytes)
    }
}

/// 找到原始 CSV 记录中插入一个空字段所需的逗号位置。
///
/// 只接受字段数与物理源列数完全一致且引号闭合的记录；字段原始字节、
/// 引号风格及 CRLF/LF/无末行换行均保持不变。引号内逗号和换行不计字段边界。
pub(super) fn raw_empty_field_insert_offset(
    record: &[u8],
    expected_fields: usize,
    insert_col: usize,
) -> Option<usize> {
    if expected_fields == 0 || insert_col > expected_fields {
        return None;
    }
    let mut content_end = record.len();
    if content_end > 0 && record[content_end - 1] == b'\n' {
        content_end -= 1;
    }
    if content_end > 0 && record[content_end - 1] == b'\r' {
        content_end -= 1;
    }
    let mut insertion = (insert_col == 0).then_some(0);
    let mut in_quotes = false;
    let mut field = 0usize;
    for (index, byte) in record[..content_end].iter().copied().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                field += 1;
                if field >= expected_fields {
                    return None;
                }
                if field == insert_col {
                    insertion = Some(index + 1);
                }
            }
            _ => {}
        }
    }
    if in_quotes || field + 1 != expected_fields {
        return None;
    }
    if insert_col == expected_fields {
        Some(content_end)
    } else {
        insertion
    }
}

/// 识别“原 Source 列顺序不变，只增加一个空 Inserted 列”的候选布局。
fn single_empty_insert_column(document: &BigFile, schema: &Schema) -> Option<usize> {
    if document.format != DocumentFormat::Csv
        || document.output_encoding != document.backing_encoding
        || document.base_ncols == 0
        || schema.visible().len() != document.base_ncols.checked_add(1)?
    {
        return None;
    }
    let mut insert_col = None;
    let mut next_source = 0usize;
    for (slot, id) in schema.visible().iter().copied().enumerate() {
        match schema.storage(id) {
            Some(ColumnStorage::Source(source)) if source as usize == next_source => {
                next_source += 1;
            }
            Some(ColumnStorage::Inserted(_)) if insert_col.is_none() => {
                insert_col = Some(slot);
            }
            _ => return None,
        }
    }
    if next_source == document.base_ncols {
        insert_col
    } else {
        None
    }
}

/// 优先按原始偏移插入空字段；不规则记录回退到共享编码器。
/// 返回值表示是否写出了缺少末尾换行的原始记录，供排序分支补记录分隔符。
fn write_source_record<S: DocumentSink>(
    document: &BigFile,
    schema: &Schema,
    rec: u32,
    raw_insert_col: Option<usize>,
    output: &mut S,
) -> Result<bool, String> {
    if let Some(insert_col) = raw_insert_col {
        if let Some((start, end)) = document.src_span(rec) {
            let raw = &document.bytes()[start..end];
            if let Some(at) = raw_empty_field_insert_offset(raw, document.base_ncols, insert_col) {
                output.write_all(&raw[..at])?;
                output.write_all(b",")?;
                output.write_all(&raw[at..])?;
                return Ok(!raw.ends_with(b"\n"));
            }
        }
    }
    let fields = document.src_fields_with_schema(rec, schema);
    output.write_all(&encode_document_record(
        &fields,
        b',',
        document.format,
        document.output_encoding,
    )?)?;
    Ok(false)
}

/// 写出连续物理源记录区间，保留原始引号、空白和换行风格。
fn write_raw_range<S: DocumentSink>(
    document: &BigFile,
    sink: &mut S,
    first: u32,
    end: u32,
) -> Result<(), String> {
    let bytes = document.bytes();
    let mut start = document.idx.record_span(bytes.len(), first as usize).0;
    let end = if (end as usize) < document.idx.n_records() {
        document.idx.record_span(bytes.len(), end as usize).0
    } else {
        bytes.len()
    };
    // BOM 已由共享入口单独写出；前导空行存在时不能盲目跳过文件头三字节。
    if first == 0 && document.has_bom && start == 0 {
        start = 3;
    }
    sink.write_all(&bytes[start..end])
}

/// 一次物化期间只构建一次的共享序列化状态。
///
/// 并行分块只读取该结构；编辑行集合与单空列插入信息必须共享，
/// 否则每个分块都会复制一份随编辑数增长的 `HashSet`。
pub(super) struct SerializationContext<'a> {
    view: SerializationView<'a>,
    edited: std::collections::HashSet<u32>,
    must_encode_source: bool,
    raw_insert_col: Option<usize>,
    total: u64,
    /// 每个段在逻辑行序列中的起始位置，长度等于段数加一。
    seg_starts: Vec<u64>,
}

impl<'a> SerializationContext<'a> {
    /// 冻结视图并一次性构建跨分块共享的派生状态。
    pub(super) fn new(view: SerializationView<'a>) -> Self {
        let document = view.document;
        let schema = view.schema;
        let edited: std::collections::HashSet<u32> =
            document.overlay.keys().map(|key| key.0).collect();
        let source_layout_changed = schema.visible().len() != document.base_ncols
            || schema.visible().iter().enumerate().any(|(slot, id)| {
                u32::try_from(slot)
                    .map(|slot| schema.storage(*id) != Some(ColumnStorage::Source(slot)))
                    .unwrap_or(true)
            });
        let encoding_changed = document.output_encoding != document.backing_encoding;
        let must_encode_source = source_layout_changed || encoding_changed;
        let raw_insert_col = single_empty_insert_column(document, schema);
        let total = match view.sorted {
            Some(order) => order.as_slice().len() as u64,
            None => document.seg.vrow_count(),
        };
        let mut seg_starts = Vec::with_capacity(document.seg.segs.len() + 1);
        let mut cursor = 0u64;
        seg_starts.push(0);
        for segment in &document.seg.segs {
            cursor += segment.len as u64;
            seg_starts.push(cursor);
        }
        Self {
            view,
            edited,
            must_encode_source,
            raw_insert_col,
            total,
            seg_starts,
        }
    }

    /// 返回本次物化的逻辑数据行总数。
    pub(super) fn total_rows(&self) -> u64 {
        self.total
    }

    /// 序列化一个逻辑数据行区间，并在每条数据行后报告区间内完成行数。
    ///
    /// # Parameters
    /// - `sink`: 正文字节写出器。
    /// - `span`: 逻辑数据行区间，坐标基于整篇文档。
    /// - `include_prefix`: 是否写出 BOM 和表头；必须且只能由 `start == 0` 的区间承担。
    /// - `on_rows`: 每写出一条数据行，以区间内已完成行数回调一次。
    ///
    /// # Returns
    /// 该区间实际写出的字节统计；`line_capacity` 从 1 起算，跨区间合并时使用
    /// [`SerializationStats::combine`]。
    ///
    /// # Errors
    /// 区间越界、前缀归属错误或底层写出失败时返回错误。
    pub(super) fn serialize_row_range<S: DocumentSink>(
        &self,
        sink: &mut S,
        span: RowSpan,
        include_prefix: bool,
        mut on_rows: impl FnMut(u64),
    ) -> Result<SerializationStats, String> {
        if span.end < span.start || span.end > self.total {
            return Err(format!(
                "物化行区间越界：{}..{}，总行数 {}",
                span.start, span.end, self.total,
            ));
        }
        if include_prefix != (span.start == 0) {
            return Err("表头前缀必须且只能随首个行区间写入".into());
        }
        let SerializationView {
            document,
            schema,
            saved_headers,
            sorted,
        } = self.view;
        let mut output = ObservedSink {
            inner: sink,
            counter: IndexCounter::new(document.format),
        };
        if include_prefix {
            if document.has_bom {
                output.write_all(&[0xEF, 0xBB, 0xBF])?;
            }
            if !document.headerless && !document.format.is_text_lines() {
                if document.output_encoding == document.backing_encoding
                    && saved_headers == document.base_headers.as_slice()
                    && document.raw_src_fields(0) == saved_headers
                {
                    // 只有物理记录 0 仍表示当前表头时才透传；首行模式切换可能已提升另一行。
                    write_raw_range(document, &mut output, 0, 1)?;
                } else {
                    output.write_all(&encode_document_record(
                        saved_headers,
                        b',',
                        document.format,
                        document.output_encoding,
                    )?)?;
                }
            }
        }

        let mut done = 0u64;
        if let Some(order) = sorted {
            let rows = &order.as_slice()[span.start as usize..span.end as usize];
            for &orig in rows {
                match document.seg.resolve(orig as u64) {
                    Some(ResolvedRow::Src(rec)) => {
                        if self.must_encode_source || self.edited.contains(&rec) {
                            let raw_without_newline = write_source_record(
                                document,
                                schema,
                                rec,
                                self.raw_insert_col
                                    .filter(|_| !self.edited.contains(&rec)),
                                &mut output,
                            )?;
                            // 原始末行没有换行时，排序物化后仍需与下一条记录分隔。
                            if raw_without_newline {
                                output.write_all(b"\r\n")?;
                            }
                        } else {
                            let (start, end) = document.src_span(rec).unwrap_or((0, 0));
                            let raw = &document.bytes()[start..end];
                            output.write_all(raw)?;
                            // 原末行没有换行时，排序物化后仍需与下一条记录分隔。
                            if !raw.ends_with(b"\n") {
                                output.write_all(b"\r\n")?;
                            }
                        }
                    }
                    Some(ResolvedRow::Ins { block, row }) => {
                        let fields = document.ins_fields_with_schema(block, row, schema);
                        output.write_all(&encode_document_record(
                            &fields,
                            b',',
                            document.format,
                            document.output_encoding,
                        )?)?;
                    }
                    None => {}
                }
                done += 1;
                on_rows(done);
            }
            return output.counter.finish(done);
        }

        let segments = &document.seg.segs;
        // seg_starts 是段起始位置前缀和；定位首个与区间相交的段。
        let first = self
            .seg_starts
            .partition_point(|&start| start <= span.start)
            .saturating_sub(1);
        for index in first..segments.len() {
            let seg_start = self.seg_starts[index];
            if seg_start >= span.end {
                break;
            }
            let seg_end = self.seg_starts[index + 1];
            let from = span.start.max(seg_start) - seg_start;
            let to = span.end.min(seg_end) - seg_start;
            let from_u32 = u32::try_from(from).map_err(|_| "段内起始行超过 u32")?;
            let to_u32 = u32::try_from(to).map_err(|_| "段内结束行超过 u32")?;
            match segments[index].kind {
                SegKind::Src { start, .. } => {
                    let rec_from = start + from_u32;
                    let rec_to = start + to_u32;
                    let mut raw_run = None;
                    for rec in rec_from..rec_to {
                        if self.must_encode_source || self.edited.contains(&rec) {
                            if let Some(first) = raw_run.take() {
                                write_raw_range(document, &mut output, first, rec)?;
                            }
                            write_source_record(
                                document,
                                schema,
                                rec,
                                self.raw_insert_col.filter(|_| !self.edited.contains(&rec)),
                                &mut output,
                            )?;
                        } else if raw_run.is_none() {
                            raw_run = Some(rec);
                        }
                        done += 1;
                        on_rows(done);
                    }
                    if let Some(first) = raw_run.take() {
                        write_raw_range(document, &mut output, first, rec_to)?;
                    }
                }
                SegKind::Ins { block } => {
                    if let Some(Some(rows)) = document.seg.blocks.get(block as usize) {
                        let limit = to.min(rows.rows.len() as u64);
                        for row_index in from..limit {
                            let row = u32::try_from(row_index).map_err(|_| "插入行号越界")?;
                            let fields = document.ins_fields_with_schema(block, row, schema);
                            output.write_all(&encode_document_record(
                                &fields,
                                b',',
                                document.format,
                                document.output_encoding,
                            )?)?;
                            done += 1;
                            on_rows(done);
                        }
                    }
                }
            }
        }
        output.counter.finish(done)
    }
}

/// 使用冻结视图序列化完整正文，并在每条逻辑数据行后报告进度。
pub(super) fn serialize_document<S: DocumentSink>(
    view: SerializationView<'_>,
    sink: &mut S,
    on_rows: impl FnMut(u64, u64),
) -> Result<SerializationStats, String> {
    let context = SerializationContext::new(view);
    let total = context.total_rows();
    let mut on_rows = on_rows;
    context.serialize_row_range(
        sink,
        RowSpan { start: 0, end: total },
        true,
        |done| on_rows(done, total),
    )
}
