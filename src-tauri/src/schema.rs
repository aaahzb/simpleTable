use crate::history::Cost;
use std::collections::BTreeMap;

/// 会话内稳定列身份；与物理存储槽和当前可见位置无关。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnId(pub u32);

/// 列内容的物理存储来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnStorage {
    Source(u32),
    Inserted(u32),
}

/// 一个稳定列身份对应的存储来源和列名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub storage: ColumnStorage,
    pub header: String,
}

/// 当前可见列顺序及所有仍被文档或历史引用的列身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    visible: Vec<ColumnId>,
    columns: BTreeMap<ColumnId, Column>,
    next_id: u32,
    next_inserted_storage: u32,
    history_bytes: u64,
}

impl Schema {
    /// 从原始表头建立连续 Source 存储和稳定 ColumnId。
    pub fn from_headers(headers: Vec<String>) -> Result<Self, String> {
        let n = u32::try_from(headers.len()).map_err(|_| "列数超过 u32".to_string())?;
        let mut visible = Vec::with_capacity(headers.len());
        let mut columns = BTreeMap::new();
        for (i, header) in headers.into_iter().enumerate() {
            let id = ColumnId(i as u32);
            visible.push(id);
            columns.insert(id, Column {
                storage: ColumnStorage::Source(i as u32),
                header,
            });
        }
        let mut schema = Self {
            visible,
            columns,
            next_id: n,
            next_inserted_storage: 0,
            history_bytes: 0,
        };
        schema.history_bytes = schema.compute_history_bytes()?;
        Ok(schema)
    }

    /// 当前可见列的稳定身份顺序。
    pub fn visible(&self) -> &[ColumnId] { &self.visible }

    /// 解析仍被文档或历史持有的列身份，包括已隐藏列。
    pub fn resolve(&self, id: ColumnId) -> Option<&Column> { self.columns.get(&id) }

    /// 查询稳定列身份的物理存储来源。
    pub fn storage(&self, id: ColumnId) -> Option<ColumnStorage> {
        self.resolve(id).map(|column| column.storage)
    }

    /// 将当前视图列号解析为稳定身份。
    pub fn view_id(&self, view_col: u32) -> Option<ColumnId> {
        self.visible.get(view_col as usize).copied()
    }

    /// 按实际 Vec/String 容量和 BTreeMap 节点估算共享历史成本。
    fn compute_history_bytes(&self) -> Result<u64, String> {
        let mut cost = Cost::new();
        cost.add_value::<Self>().map_err(|e| format!("{e:?}"))?;
        cost.add_vec(&self.visible).map_err(|e| format!("{e:?}"))?;
        for column in self.columns.values() {
            cost.add_value::<(ColumnId, Column)>().map_err(|e| format!("{e:?}"))?;
            cost.add((6 * std::mem::size_of::<usize>()) as u64)
                .map_err(|e| format!("{e:?}"))?;
            cost.add_string(&column.header).map_err(|e| format!("{e:?}"))?;
        }
        Ok(cost.bytes())
    }

    /// 候选结构完整核算成功后才替换活动 Schema。
    fn finish_candidate(mut candidate: Self) -> Result<Self, String> {
        candidate.history_bytes = candidate.compute_history_bytes()?;
        Ok(candidate)
    }

    /// 在可见位置插入新身份和 Inserted 存储槽。
    pub fn insert(&mut self, pos: u32, header: String) -> Result<ColumnId, String> {
        let pos = usize::try_from(pos).map_err(|_| "插入列位置无效".to_string())?;
        if pos > self.visible.len() {
            return Err("插入列位置越界".into());
        }
        let mut next = self.clone();
        let id = ColumnId(next.next_id);
        next.next_id = next.next_id.checked_add(1).ok_or("ColumnId 溢出")?;
        let storage = next.next_inserted_storage;
        next.next_inserted_storage = next.next_inserted_storage.checked_add(1)
            .ok_or("插入列存储编号溢出")?;
        next.visible.insert(pos, id);
        next.columns.insert(id, Column {
            storage: ColumnStorage::Inserted(storage),
            header,
        });
        *self = Self::finish_candidate(next)?;
        Ok(id)
    }

    /// 只隐藏可见映射，保留存储身份供历史恢复。
    pub fn delete(&mut self, id: ColumnId) -> Result<(), String> {
        if !self.visible.contains(&id) {
            return Err("列不在当前可见结构中".into());
        }
        if self.visible.len() == 1 {
            return Err("至少保留一列".into());
        }
        let mut next = self.clone();
        next.visible.retain(|current| *current != id);
        *self = Self::finish_candidate(next)?;
        Ok(())
    }

    /// pending 预览只隐藏身份，允许暂时零列；物理 apply 再执行零列守卫。
    pub(crate) fn delete_preview(&mut self, id: ColumnId) -> Result<(), String> {
        if !self.visible.contains(&id) {
            return Err("列不在当前可见结构中".into());
        }
        if self.visible.len() > 1 {
            return self.delete(id);
        }
        let mut next = self.clone();
        next.visible.retain(|current| *current != id);
        *self = Self::finish_candidate(next)?;
        Ok(())
    }

    /// 修改稳定身份的列名，不改变存储或可见位置。
    pub fn rename(&mut self, id: ColumnId, header: String) -> Result<(), String> {
        if !self.columns.contains_key(&id) {
            return Err("列身份不存在".into());
        }
        let mut next = self.clone();
        next.columns.get_mut(&id).unwrap().header = header;
        *self = Self::finish_candidate(next)?;
        Ok(())
    }

    /// 预先核算的共享 Schema 历史字节数。
    /// Task 9 接入 Op::Columns 共享计费后移除临时豁免。
    #[allow(dead_code)]
    pub fn history_bytes(&self) -> u64 { self.history_bytes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::SharedCharge;
    use std::sync::Arc;

    #[test]
    fn schema_ids_survive_insert_delete_rename_and_reorder() {
        let mut schema = Schema::from_headers(vec!["A".into(), "B".into(), "C".into()]).unwrap();
        let a: ColumnId = schema.visible()[0];
        let b: ColumnId = schema.visible()[1];
        let inserted = schema.insert(1, "X".into()).unwrap();
        assert_eq!(schema.visible(), &[a, inserted, b, schema.visible()[3]]);
        schema.rename(b, "Bee".into()).unwrap();
        schema.delete(a).unwrap();
        assert_eq!(schema.resolve(b).unwrap().header, "Bee");
        assert_eq!(schema.storage(b), Some(ColumnStorage::Source(1)));
        assert!(!schema.visible().contains(&a));
        assert!(schema.resolve(a).is_some());
    }

    #[test]
    fn schema_history_charge_does_not_depend_on_document_rows() {
        let schema = Arc::new(Schema::from_headers(vec!["A".into(), "B".into()]).unwrap());
        let before = schema.history_bytes();
        let same = schema.clone();
        assert_eq!(before, same.history_bytes());
        assert_eq!(SharedCharge::of(&schema, before), SharedCharge::of(&same, before));
    }
}
