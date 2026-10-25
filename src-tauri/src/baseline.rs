use crate::schema::ColumnId;
use std::collections::{BTreeMap, BTreeSet};

/// 历史使用的稳定行身份；Source 为当前 backing 的物理记录号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowId {
    Source(u32),
    Inserted { block: u32, row: u32 },
}

/// 连续幸存源记录的半开区间及其新起点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordRange {
    pub old_start: u32,
    pub old_end: u32,
    pub new_start: u32,
}

/// 普通保存前后的稀疏身份映射，大小取决于段数而非文档行数。
#[derive(Clone, Debug, Default)]
pub struct BaselineTransform {
    pub ranges: Vec<RecordRange>,
    pub inserted_rows: BTreeMap<(u32, u32), u32>,
    pub deleted_sources: BTreeSet<u32>,
    /// 已删除 Source rec 到预留 Inserted block（row=0）的稳定映射。
    pub suspended_rows: BTreeMap<u32, u32>,
    /// 当前可见旧 ColumnId 到保存后连续 Source 槽/新 ColumnId 的映射。
    pub columns: BTreeMap<ColumnId, u32>,
    /// 保存前所有仍保留的物理槽到稳定列身份，含当前隐藏列。
    pub source_columns: BTreeMap<u32, ColumnId>,
    pub inserted_columns: BTreeMap<u32, ColumnId>,
}

impl BaselineTransform {
    /// 追加一条幸存源记录；只有旧、新记录号都连续时才合并区间。
    pub fn push_surviving_source(&mut self, old_rec: u32, new_rec: u32)
        -> Result<(), String> {
        let old_end = old_rec.checked_add(1).ok_or("源记录号溢出")?;
        if let Some(last) = self.ranges.last_mut() {
            if old_rec < last.old_end {
                return Err("源记录区间重叠或倒序".into());
            }
            let last_new_end = last.new_start
                .checked_add(last.old_end - last.old_start)
                .ok_or("新记录号溢出")?;
            if new_rec < last_new_end {
                return Err("新记录区间重叠或倒序".into());
            }
            if last.old_end == old_rec && last_new_end == new_rec {
                last.old_end = old_end;
                return Ok(());
            }
        }
        self.ranges.push(RecordRange { old_start: old_rec, old_end, new_start: new_rec });
        Ok(())
    }

    /// 判断保存前后的物理记录布局是否完全恒等。
    ///
    /// 仅恒等布局允许排列原样重绑定；存在插入、删除、悬挂或范围位移时返回 false。
    pub fn row_layout_is_identity(&self) -> bool {
        self.inserted_rows.is_empty()
            && self.deleted_sources.is_empty()
            && self.suspended_rows.is_empty()
            && self.ranges.iter().all(|range| range.old_start == range.new_start)
    }

    /// 将旧 Source 记录号映射为保存后的物理记录号；删除或未知记录不映射。
    pub fn map_source(&self, old_rec: u32) -> Option<u32> {
        let index = self.ranges.partition_point(|range| range.old_start <= old_rec);
        if index == 0 {
            return None;
        }
        let range = &self.ranges[index - 1];
        if old_rec >= range.old_end {
            return None;
        }
        range.new_start.checked_add(old_rec - range.old_start)
    }
}

#[cfg(test)]
mod tests {
    use super::BaselineTransform;

    #[test]
    fn baseline_ranges_require_old_and_new_contiguity() {
        let mut transform = BaselineTransform::default();
        for rec in 0..100_000 {
            transform.push_surviving_source(rec, rec).unwrap();
        }
        assert_eq!(transform.ranges.len(), 1);
        transform.push_surviving_source(100_001, 100_000).unwrap();
        assert_eq!(transform.ranges.len(), 2);
        transform.push_surviving_source(100_002, 100_002).unwrap();
        assert_eq!(transform.ranges.len(), 3);
        assert_eq!(transform.map_source(100_001), Some(100_000));
        assert_eq!(transform.map_source(100_000), None);
    }

    #[test]
    fn baseline_identity_row_layout_is_detected() {
        let mut transform = BaselineTransform::default();
        transform.push_surviving_source(0, 0).unwrap();
        transform.push_surviving_source(1, 1).unwrap();
        assert!(transform.row_layout_is_identity());
        transform.inserted_rows.insert((7, 2), 12);
        assert!(!transform.row_layout_is_identity());
    }

    #[test]
    fn baseline_tracks_sparse_inserted_and_deleted_sources() {
        let mut transform = BaselineTransform::default();
        transform.inserted_rows.insert((7, 2), 12);
        transform.deleted_sources.insert(41);
        assert_eq!(transform.inserted_rows.get(&(7, 2)), Some(&12));
        assert!(transform.deleted_sources.contains(&41));
        assert_eq!(transform.map_source(41), None);
    }
}
