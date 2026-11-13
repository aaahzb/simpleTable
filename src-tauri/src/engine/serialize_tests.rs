use super::serialize::{
    raw_empty_field_insert_offset, serialize_document, CountingSink, DocumentSink,
    RamRegionSink, RowSpan, SerializationContext, SerializationStats, SerializationView,
};
use super::{BigFile, ColOp};
use crate::history::Approval;
use crate::order::Perm;
use crate::ram_region::RamRegion;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

/// 测试用内存写出器：记录完整正文字节，便于与顺序路径逐字节比较。
#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
}

impl DocumentSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

/// 把总行数尽量均分为不超过 `chunks` 个非空区间。
fn split_spans(total: u64, chunks: usize) -> Vec<RowSpan> {
    let count = chunks.min(total as usize).max(1);
    let base = total / count as u64;
    let rem = total % count as u64;
    let mut spans = Vec::with_capacity(count);
    let mut start = 0u64;
    for index in 0..count {
        let len = base + u64::from((index as u64) < rem);
        // 空文档只产生一个空区间，用于验证表头前缀路径。
        if total == 0 {
            spans.push(RowSpan { start: 0, end: 0 });
            break;
        }
        spans.push(RowSpan {
            start,
            end: start + len,
        });
        start += len;
    }
    spans
}


/// 创建独立的合成文档路径，并写入指定原始字节。
fn fixture_path(name: &str, extension: &str, bytes: &[u8]) -> PathBuf {
    let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "simpletable_serialize_{name}_{}_{id}.{extension}",
        std::process::id(),
    ));
    fs::write(&path, bytes).unwrap();
    path
}

/// 从合成字节打开真实文档，不走内存伪造后备。
fn open_fixture(name: &str, extension: &str, bytes: &[u8]) -> BigFile {
    let path = fixture_path(name, extension, bytes);
    BigFile::open(&path).unwrap()
}

#[test]
fn shared_serializer_clean_format_matrix_is_byte_exact() {
    let gbk = encoding_rs::GBK
        .encode("name,note\r\nAlice,北京\r\n")
        .0
        .into_owned();
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("utf8-crlf", "csv", b"name,note\r\nAlice,x\r\n".to_vec()),
        ("utf8-bom", "csv", b"\xEF\xBB\xBFname,note\nAlice,x\n".to_vec()),
        (
            "quotes-multiline",
            "csv",
            b"name,note\r\nAlice,\"a,b\nline2\"\r\n".to_vec(),
        ),
        (
            "no-final-newline",
            "csv",
            b"name,age\r\nAlice,30".to_vec(),
        ),
        ("lf", "csv", b"name,age\nAlice,30\n".to_vec()),
        ("gbk", "csv", gbk),
        (
            "text-lines",
            "txt",
            "北京\n\nAlice\n".as_bytes().to_vec(),
        ),
    ];
    for (name, extension, expected) in cases {
        let bf = open_fixture(name, extension, &expected);
        let path = bf.path.clone();
        let headers = bf.view_headers();
        let mut counting = CountingSink::new();
        let counted = serialize_document(
            SerializationView {
                document: &bf,
                schema: &bf.schema,
                saved_headers: &headers,
                sorted: None,
            },
            &mut counting,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(counted.bytes, expected.len() as u64, "{name}");

        let mut ram_sink = RamRegionSink::new(RamRegion::reserve(expected.len()).unwrap());
        let written = serialize_document(
            SerializationView {
                document: &bf,
                schema: &bf.schema,
                saved_headers: &headers,
                sorted: None,
            },
            &mut ram_sink,
            |_, _| {},
        )
        .unwrap();
        let ram = ram_sink.finish().unwrap();
        assert_eq!(written, counted, "{name}");
        assert_eq!(ram.as_slice(), expected.as_slice(), "{name}");
        drop(ram);
        drop(bf);
        fs::remove_file(path).ok();
    }
}

#[test]
fn shared_serializer_reencodes_edits_with_candidate_schema() {
    let p = fixture_path("candidate-schema", "csv", b"name,age\r\nAlice,30\r\n");
    let mut bf = BigFile::open(&p).unwrap();
    bf.set_cell(0, 1, "北 京,8".into(), Approval::deny())
        .unwrap();
    bf.push_col_op(ColOp::Insert {
        pos: 1,
        name: "empty".into(),
    })
    .unwrap();
    let candidate = bf.pending_schema.clone().unwrap();
    let headers = candidate
        .visible()
        .iter()
        .map(|id| candidate.resolve(*id).unwrap().header.clone())
        .collect::<Vec<_>>();
    let mut counting = CountingSink::new();
    serialize_document(
        SerializationView {
            document: &bf,
            schema: &candidate,
            saved_headers: &headers,
            sorted: None,
        },
        &mut counting,
        |_, _| {},
    )
    .unwrap();

    let mut ram_sink = RamRegionSink::new(RamRegion::reserve(counting.bytes()).unwrap());
    serialize_document(
        SerializationView {
            document: &bf,
            schema: &candidate,
            saved_headers: &headers,
            sorted: None,
        },
        &mut ram_sink,
        |_, _| {},
    )
    .unwrap();
    let ram = ram_sink.finish().unwrap();
    assert_eq!(
        ram.as_slice(),
        "name,empty,age\r\nAlice,,\"北 京,8\"\r\n".as_bytes()
    );
    drop(ram);
    drop(bf);
    fs::remove_file(p).ok();
}

#[test]
fn shared_serializer_sorted_missing_newline_adds_record_separator() {
    let p = fixture_path("sorted-missing-newline", "csv", b"id,v\r\nb,2\r\na,1");
    let bf = BigFile::open(&p).unwrap();
    let headers = bf.view_headers();
    let sorted = Perm::new(vec![1, 0]).unwrap();
    let expected = b"id,v\r\na,1\r\nb,2\r\n";
    let mut counting = CountingSink::new();
    let counted = serialize_document(
        SerializationView {
            document: &bf,
            schema: &bf.schema,
            saved_headers: &headers,
            sorted: Some(&sorted),
        },
        &mut counting,
        |_, _| {},
    )
    .unwrap();
    assert_eq!(counted.bytes, expected.len() as u64);

    let mut ram_sink = RamRegionSink::new(RamRegion::reserve(expected.len()).unwrap());
    let written = serialize_document(
        SerializationView {
            document: &bf,
            schema: &bf.schema,
            saved_headers: &headers,
            sorted: Some(&sorted),
        },
        &mut ram_sink,
        |_, _| {},
    )
    .unwrap();
    assert_eq!(written, counted);
    assert_eq!(ram_sink.finish().unwrap().as_slice(), expected);
    drop(bf);
    fs::remove_file(p).ok();
}

/// 与整篇序列化共享的格式夹具，覆盖 BOM、GBK、引号多行、缺末尾换行与纯文本。
fn serializer_fixture_cases() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let gbk = encoding_rs::GBK
        .encode("name,note\r\nAlice,北京\r\n")
        .0
        .into_owned();
    vec![
        ("range-utf8-crlf", "csv", b"name,note\r\nAlice,x\r\nbob,y\r\n".to_vec()),
        (
            "range-utf8-bom",
            "csv",
            b"\xEF\xBB\xBFname,note\nAlice,x\nbob,y\n".to_vec(),
        ),
        (
            "range-quotes-multiline",
            "csv",
            b"name,note\r\nAlice,\"a,b\nline2\"\r\nbob,z\r\n".to_vec(),
        ),
        (
            "range-no-final-newline",
            "csv",
            b"name,age\r\nAlice,30\r\nbob,40".to_vec(),
        ),
        ("range-lf", "csv", b"name,age\nAlice,30\nbob,40\n".to_vec()),
        ("range-gbk", "csv", gbk),
        (
            "range-text-lines",
            "txt",
            "北京\n\nAlice\n".as_bytes().to_vec(),
        ),
    ]
}

#[test]
fn serialize_row_range_matches_full_document_for_every_chunk_partition() {
    for (name, extension, expected) in serializer_fixture_cases() {
        let bf = open_fixture(name, extension, &expected);
        let path = bf.path.clone();
        let headers = bf.view_headers();
        let view = SerializationView {
            document: &bf,
            schema: &bf.schema,
            saved_headers: &headers,
            sorted: None,
        };
        let context = SerializationContext::new(view);
        let total = context.total_rows();
        assert!(total > 0, "{name}");

        let mut baseline = VecSink::default();
        let baseline_stats = serialize_document(view, &mut baseline, |_, _| {}).unwrap();
        assert_eq!(baseline.bytes, expected, "{name}");

        let mut whole = VecSink::default();
        let whole_stats = context
            .serialize_row_range(
                &mut whole,
                RowSpan {
                    start: 0,
                    end: total,
                },
                true,
                |_| {},
            )
            .unwrap();
        assert_eq!(whole.bytes, baseline.bytes, "{name} 整区间字节");
        assert_eq!(whole_stats, baseline_stats, "{name} 整区间统计");

        for chunks in [1usize, 2, 3, 7] {
            let spans = split_spans(total, chunks);
            let mut joined = VecSink::default();
            let mut combined: Option<SerializationStats> = None;
            for (index, span) in spans.iter().copied().enumerate() {
                let mut part = VecSink::default();
                let mut local = Vec::new();
                let stats = context
                    .serialize_row_range(&mut part, span, index == 0, |done| {
                        local.push(done);
                    })
                    .unwrap();
                let expected_local: Vec<u64> = (1..=span.end - span.start).collect();
                assert_eq!(local, expected_local, "{name} chunks={chunks} 行进度");
                joined.bytes.extend_from_slice(&part.bytes);
                combined = Some(match combined {
                    None => stats,
                    Some(previous) => previous.combine(stats).unwrap(),
                });
            }
            assert_eq!(
                joined.bytes, baseline.bytes,
                "{name} chunks={chunks} 拼接字节"
            );
            assert_eq!(
                combined.unwrap(),
                baseline_stats,
                "{name} chunks={chunks} 合并统计"
            );
        }
        drop(bf);
        fs::remove_file(path).ok();
    }
}

#[test]
fn serialize_row_range_rejects_prefix_on_later_range_and_out_of_bounds() {
    let path = fixture_path(
        "range-errors",
        "csv",
        b"name,age\r\nAlice,30\r\nbob,40\r\n",
    );
    let bf = BigFile::open(&path).unwrap();
    let headers = bf.view_headers();
    let context = SerializationContext::new(SerializationView {
        document: &bf,
        schema: &bf.schema,
        saved_headers: &headers,
        sorted: None,
    });
    let total = context.total_rows();
    assert!(total >= 2);

    let mut sink = VecSink::default();
    let err = context
        .serialize_row_range(&mut sink, RowSpan { start: 1, end: 2 }, true, |_| {})
        .unwrap_err();
    assert!(err.contains("表头前缀"), "{err}");
    let err = context
        .serialize_row_range(
            &mut sink,
            RowSpan {
                start: 0,
                end: total + 1,
            },
            true,
            |_| {},
        )
        .unwrap_err();
    assert!(err.contains("越界"), "{err}");
    let err = context
        .serialize_row_range(&mut sink, RowSpan { start: 2, end: 1 }, true, |_| {})
        .unwrap_err();
    assert!(err.contains("越界"), "{err}");
    drop(bf);
    fs::remove_file(path).ok();
}

#[test]
fn serialization_stats_combine_restores_full_line_capacity() {
    let first = SerializationStats {
        bytes: 10,
        line_capacity: 3,
        record_capacity: 2,
        rows: 2,
    };
    let second = SerializationStats {
        bytes: 7,
        line_capacity: 4,
        record_capacity: 2,
        rows: 2,
    };
    assert_eq!(
        first.combine(second).unwrap(),
        SerializationStats {
            bytes: 17,
            line_capacity: 6,
            record_capacity: 4,
            rows: 4,
        }
    );
}

#[test]
fn parallel_serialization_shared_types_are_sync() {
    fn assert_sync<T: Sync>() {}
    assert_sync::<BigFile>();
    assert_sync::<SerializationContext<'static>>();
}

fn raw_inserted(record: &[u8], expected_fields: usize, insert_col: usize) -> Vec<u8> {
    let at = raw_empty_field_insert_offset(record, expected_fields, insert_col).unwrap();
    let mut out = Vec::with_capacity(record.len() + 1);
    out.extend_from_slice(&record[..at]);
    out.push(b',');
    out.extend_from_slice(&record[at..]);
    out
}

#[test]
fn raw_insert_offset_handles_csv_boundaries_and_rejects_irregular_records() {
    assert_eq!(raw_inserted(b"a,b\r\n", 2, 0), b",a,b\r\n");
    assert_eq!(raw_inserted(b"a,b\r\n", 2, 1), b"a,,b\r\n");
    assert_eq!(raw_inserted(b"a,b\r\n", 2, 2), b"a,b,\r\n");
    assert_eq!(raw_inserted(b"\"a,b\",c\n", 2, 1), b"\"a,b\",,c\n");
    assert_eq!(raw_inserted(b"\"a\nx\",b\r\n", 2, 2), b"\"a\nx\",b,\r\n");
    assert_eq!(raw_inserted(b"a,b", 2, 2), b"a,b,");
    assert_eq!(raw_empty_field_insert_offset(b"a,b\r\n", 1, 1), None);
    assert_eq!(raw_empty_field_insert_offset(b"a,b\r\n", 3, 1), None);
    assert_eq!(raw_empty_field_insert_offset(b"\"unclosed,b", 2, 1), None);
}
