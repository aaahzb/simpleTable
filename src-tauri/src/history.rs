use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

pub const DEFAULT_HISTORY_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_RESIDENT_PERMILLE: u32 = 1000;
pub const MAX_RESIDENT_PERMILLE: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoPolicy {
    pub budget_bytes: u64,
    pub resident_permille: u32,
}

impl Default for UndoPolicy {
    fn default() -> Self {
        Self {
            budget_bytes: DEFAULT_HISTORY_BUDGET_BYTES,
            resident_permille: DEFAULT_RESIDENT_PERMILLE,
        }
    }
}

/// 内存模式有效预算 = max(普通预算, ceil(file_bytes × permille / 1000))。
pub fn effective_undo_budget_bytes(
    file_bytes: u64,
    memory_mode: bool,
    policy: UndoPolicy,
) -> Result<u64, String> {
    if policy.resident_permille > MAX_RESIDENT_PERMILLE {
        return Err("驻留撤销比例超出 0%–1000% 范围".into());
    }
    if !memory_mode {
        return Ok(policy.budget_bytes);
    }
    let scaled = file_bytes
        .checked_mul(policy.resident_permille as u64)
        .and_then(|value| value.checked_add(999))
        .map(|value| value / 1000)
        .ok_or_else(|| "驻留撤销预算计算溢出".to_string())?;
    Ok(policy.budget_bytes.max(scaled))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    CostOverflow,
    BudgetExceeded,
    AllocationFailed,
    SharedCostConflict,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost(u64);

impl Cost {
    pub fn new() -> Self { Self(0) }
    pub fn bytes(self) -> u64 { self.0 }
    pub fn add(&mut self, bytes: u64) -> Result<(), HistoryError> {
        self.0 = self.0.checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
        Ok(())
    }
    pub fn add_value<T>(&mut self) -> Result<(), HistoryError> {
        self.add(std::mem::size_of::<T>() as u64)
    }
    pub fn add_string(&mut self, value: &String) -> Result<(), HistoryError> {
        self.add(value.capacity() as u64)
    }
    pub fn add_logical_str(&mut self, value: &str) -> Result<(), HistoryError> {
        self.add(value.len() as u64)
    }
    pub fn add_vec<T>(&mut self, value: &Vec<T>) -> Result<(), HistoryError> {
        let bytes = (value.capacity() as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .ok_or(HistoryError::CostOverflow)?;
        self.add(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedCharge { id: u64, bytes: u64 }

impl SharedCharge {
    pub fn of<T: Sized>(value: &Arc<T>, bytes: u64) -> Self {
        Self { id: Arc::as_ptr(value) as usize as u64, bytes }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub allow_fixed_point: bool,
    pub preflight_id: Option<u64>,
}

#[cfg(test)]
impl Approval {
    pub fn deny() -> Self { Self::default() }
    pub fn allow(preflight_id: u64) -> Self {
        Self { allow_fixed_point: true, preflight_id: Some(preflight_id) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FixedPointKind {
    StepOverBudget,
    SaveTransform,
    SortMaterialization,
    MemoryModeExit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedPointRequest {
    pub kind: FixedPointKind,
    pub operation: String,
    pub required_bytes: u64,
    pub budget_bytes: u64,
    pub preflight_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    pub changed: bool,
    pub inserted_rows: u64,
    pub removed_rows: u64,
    pub fixed_point: Option<FixedPointRequest>,
    pub cancelled: bool,
}

impl EditOutcome {
    pub fn unchanged() -> Self {
        Self { changed: false, inserted_rows: 0, removed_rows: 0,
            fixed_point: None, cancelled: false }
    }
}

#[derive(Clone, Debug)]
pub struct HistoryEntry<T> {
    pub op: T,
    pub own_bytes: u64,
    pub revision_delta: i64,
    shared: Vec<SharedCharge>,
}

impl<T> HistoryEntry<T> {
    pub fn new(op: T, own_bytes: u64, shared: Vec<SharedCharge>, revision_delta: i64) -> Self {
        Self { op, own_bytes, revision_delta, shared }
    }
}

pub struct Prepared<T> {
    entry: HistoryEntry<T>,
    entry_bytes: u64,
    budget_bytes: u64,
}
impl<T> Prepared<T> {
    pub fn entry(&self) -> &HistoryEntry<T> { &self.entry }
    /// 执行期只归一化块号等寻址字段；调用方不得改变已预检的字节成本。
    pub fn entry_mut(&mut self) -> &mut HistoryEntry<T> { &mut self.entry }
    pub fn entry_bytes(&self) -> u64 { self.entry_bytes }
    pub fn requires_fixed_point(&self) -> bool { self.entry_bytes > self.budget_bytes }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryStats {
    pub undo_count: usize,
    pub redo_count: usize,
    pub bytes: u64,
    pub budget_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitStats { pub evicted_undo: usize, pub cleared_redo: usize }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetChange { pub evicted: usize, pub fixed_point: bool }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetEffect {
    pub evicted_undo: usize,
    pub evicted_redo: usize,
    pub establishes_fixed_point: bool,
}

pub struct PreparedBudget<T> {
    pub(crate) ledger: HistoryLedger<T>,
    pub effect: BudgetEffect,
}

pub struct PreparedHistory<T> {
    ledger: HistoryLedger<T>,
}

#[derive(Clone)]
pub struct HistoryLedger<T> {
    undo: VecDeque<HistoryEntry<T>>,
    redo: VecDeque<HistoryEntry<T>>,
    budget_bytes: u64,
    total_bytes: u64,
    shared: HashMap<u64, (u64, u64)>, // id -> (bytes, entry_refs)
}

impl<T> HistoryLedger<T> {
    pub fn new(budget_bytes: u64) -> Self {
        Self { undo: VecDeque::new(), redo: VecDeque::new(), budget_bytes,
            total_bytes: 0, shared: HashMap::new() }
    }

    pub fn prepare(&self, entry: HistoryEntry<T>) -> Result<Prepared<T>, HistoryError> {
        let mut unique = BTreeMap::<u64, u64>::new();
        for charge in &entry.shared {
            if unique.insert(charge.id, charge.bytes)
                .is_some_and(|old| old != charge.bytes) {
                return Err(HistoryError::SharedCostConflict);
            }
        }
        let mut entry_bytes = entry.own_bytes;
        let mut incremental_bytes = entry.own_bytes;
        for (id, bytes) in unique {
            entry_bytes = entry_bytes.checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
            if let Some((old, _)) = self.shared.get(&id) {
                if *old != bytes { return Err(HistoryError::SharedCostConflict); }
            } else {
                incremental_bytes = incremental_bytes
                    .checked_add(bytes).ok_or(HistoryError::CostOverflow)?;
            }
        }
        self.total_bytes.checked_add(incremental_bytes)
            .ok_or(HistoryError::CostOverflow)?;
        Ok(Prepared { entry, entry_bytes, budget_bytes: self.budget_bytes })
    }


    pub fn iter_undo(&self) -> impl DoubleEndedIterator<Item = &HistoryEntry<T>> {
        self.undo.iter()
    }
    pub fn iter_redo(&self) -> impl DoubleEndedIterator<Item = &HistoryEntry<T>> {
        self.redo.iter()
    }
    pub fn prepare_rebuild(
        undo: VecDeque<HistoryEntry<T>>,
        redo: VecDeque<HistoryEntry<T>>,
        budget_bytes: u64,
    ) -> Result<PreparedHistory<T>, HistoryError> {
        let mut rebuilt = HistoryLedger::new(budget_bytes);
        for entry in undo.iter().chain(redo.iter()) {
            rebuilt.charge_rebuild(entry)?;
        }
        rebuilt.undo = undo;
        rebuilt.redo = redo;
        Ok(PreparedHistory { ledger: rebuilt })
    }
    /// 新文件大小确定后重建账本，并复用普通缩容的最旧淘汰规则。
    pub fn prepare_rebuild_with_eviction(
        undo: VecDeque<HistoryEntry<T>>,
        redo: VecDeque<HistoryEntry<T>>,
        budget_bytes: u64,
    ) -> Result<PreparedHistory<T>, HistoryError> {
        // 先完成完整共享计费，再统一缩容，避免转换顺序影响淘汰结果。
        let mut rebuilt = HistoryLedger::new(u64::MAX);
        for entry in undo.iter().chain(redo.iter()) {
            rebuilt.charge_rebuild(entry)?;
        }
        rebuilt.undo = undo;
        rebuilt.redo = redo;
        rebuilt.apply_budget_for_preview(budget_bytes)?;
        Ok(PreparedHistory { ledger: rebuilt })
    }
    pub fn commit_rebuild(&mut self, prepared: PreparedHistory<T>) {
        *self = prepared.ledger;
    }
    #[cfg(test)]
    pub fn undo_op(&self) -> Option<&T> { self.undo.back().map(|e| &e.op) }
    #[cfg(test)]
    pub fn redo_op(&self) -> Option<&T> { self.redo.back().map(|e| &e.op) }
    /// 重做换块后只重绑稳定格地址，不移动条目也不改变账本。
    pub fn redo_ops_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.redo.iter_mut().map(|entry| &mut entry.op)
    }
    pub fn stats(&self) -> HistoryStats {
        HistoryStats { undo_count: self.undo.len(), redo_count: self.redo.len(),
            bytes: self.total_bytes, budget_bytes: self.budget_bytes }
    }
}

impl<T> HistoryLedger<T> {

    fn charge_rebuild(&mut self, entry: &HistoryEntry<T>) -> Result<(), HistoryError> {
        let mut unique = BTreeMap::<u64, u64>::new();
        for charge in &entry.shared {
            if unique.insert(charge.id, charge.bytes)
                .is_some_and(|old| old != charge.bytes) {
                return Err(HistoryError::SharedCostConflict);
            }
        }
        let mut incremental_bytes = entry.own_bytes;
        let mut new_shared = 0usize;
        for (&id, &bytes) in &unique {
            if let Some((old, refs)) = self.shared.get(&id) {
                if *old != bytes { return Err(HistoryError::SharedCostConflict); }
                refs.checked_add(1).ok_or(HistoryError::CostOverflow)?;
            } else {
                incremental_bytes = incremental_bytes.checked_add(bytes)
                    .ok_or(HistoryError::CostOverflow)?;
                new_shared = new_shared.checked_add(1).ok_or(HistoryError::CostOverflow)?;
            }
        }
        let next_total = self.total_bytes.checked_add(incremental_bytes)
            .ok_or(HistoryError::CostOverflow)?;
        if next_total > self.budget_bytes {
            return Err(HistoryError::BudgetExceeded);
        }
        self.shared.try_reserve(new_shared).map_err(|_| HistoryError::AllocationFailed)?;
        self.total_bytes = next_total;
        for (id, bytes) in unique {
            let item = self.shared.entry(id).or_insert((bytes, 0));
            item.1 += 1;
        }
        Ok(())
    }

fn charge(&mut self, entry: &HistoryEntry<T>) {
    self.total_bytes += entry.own_bytes;
    let mut seen = BTreeSet::new();
    for charge in &entry.shared {
        if !seen.insert(charge.id) { continue; }
        let item = self.shared.entry(charge.id).or_insert((charge.bytes, 0));
        debug_assert_eq!(item.0, charge.bytes);
        if item.1 == 0 { self.total_bytes += charge.bytes; }
        item.1 += 1;
    }
}

fn release(&mut self, entry: &HistoryEntry<T>) {
    self.total_bytes -= entry.own_bytes;
    let mut seen = BTreeSet::new();
    for charge in &entry.shared {
        if !seen.insert(charge.id) { continue; }
        let item = self.shared.get_mut(&charge.id).unwrap();
        item.1 -= 1;
        if item.1 == 0 {
            self.total_bytes -= item.0;
            self.shared.remove(&charge.id);
        }
    }
}

pub fn commit_prepared(&mut self, prepared: Prepared<T>) -> CommitStats {
    debug_assert!(!prepared.requires_fixed_point());
    let mut stats = CommitStats::default();
    self.charge(&prepared.entry);
    self.undo.push_back(prepared.entry);
    while let Some(entry) = self.redo.pop_front() {
        self.release(&entry);
        stats.cleared_redo += 1;
    }
    while self.total_bytes > self.budget_bytes {
        let entry = self.undo.pop_front().unwrap();
        self.release(&entry);
        stats.evicted_undo += 1;
    }
    stats
}

pub fn take_undo(&mut self) -> Option<HistoryEntry<T>> {
    let entry = self.undo.pop_back()?;
    self.release(&entry);
    Some(entry)
}
pub fn take_redo(&mut self) -> Option<HistoryEntry<T>> {
    let entry = self.redo.pop_back()?;
    self.release(&entry);
    Some(entry)
}
pub fn finish_undo(&mut self, entry: HistoryEntry<T>) {
    self.charge(&entry);
    self.redo.push_back(entry);
}
pub fn finish_redo(&mut self, entry: HistoryEntry<T>) {
    self.charge(&entry);
    self.undo.push_back(entry);
}
pub fn restore_undo(&mut self, entry: HistoryEntry<T>) { self.finish_redo(entry); }
pub fn restore_redo(&mut self, entry: HistoryEntry<T>) { self.finish_undo(entry); }
}

impl<T> HistoryLedger<T> {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.shared.clear();
        self.total_bytes = 0;
    }

    #[cfg(test)]
    fn audited_bytes(&self) -> u64 {
        let mut expected = 0;
        let mut refs = BTreeMap::<u64, (u64, u64)>::new();
        for entry in self.undo.iter().chain(self.redo.iter()) {
            expected += entry.own_bytes;
            let mut seen = BTreeSet::new();
            for charge in &entry.shared {
                if !seen.insert(charge.id) { continue; }
                let item = refs.entry(charge.id).or_insert((charge.bytes, 0));
                assert_eq!(item.0, charge.bytes);
                item.1 += 1;
            }
        }
        assert_eq!(refs.len(), self.shared.len());
        for (id, item) in &refs { assert_eq!(self.shared.get(id), Some(item)); }
        expected + refs.values().map(|(bytes, _)| *bytes).sum::<u64>()
    }

    /// 在指定账本上执行预算淘汰；preview 与实际提交共用这一份规则。
    fn apply_budget_for_preview(
        &mut self,
        budget_bytes: u64,
    ) -> Result<BudgetEffect, HistoryError> {
        self.budget_bytes = budget_bytes;
        let mut effect = BudgetEffect::default();
        while self.total_bytes > self.budget_bytes {
            if self.undo.len() + self.redo.len() == 1 {
                self.clear();
                effect.establishes_fixed_point = true;
                break;
            }
            let from_undo = !self.undo.is_empty();
            let entry = if from_undo {
                self.undo.pop_front()
            } else {
                self.redo.pop_front()
            };
            let Some(entry) = entry else { break };
            self.release(&entry);
            if from_undo {
                effect.evicted_undo += 1;
            } else {
                effect.evicted_redo += 1;
            }
        }
        Ok(effect)
    }

    pub fn set_budget(&mut self, budget_bytes: u64) -> BudgetChange {
        let effect = self
            .apply_budget_for_preview(budget_bytes)
            .expect("预算淘汰只移动现有账本条目");
        BudgetChange {
            evicted: effect.evicted_undo + effect.evicted_redo,
            fixed_point: effect.establishes_fixed_point,
        }
    }
}

impl<T: Clone> HistoryLedger<T> {
    /// 在克隆账本上执行现有 set_budget 规则，原账本和文档现场不变。
    pub fn prepare_budget_change(
        &self,
        budget_bytes: u64,
    ) -> Result<PreparedBudget<T>, HistoryError> {
        let mut ledger = self.clone();
        let effect = ledger.apply_budget_for_preview(budget_bytes)?;
        Ok(PreparedBudget { ledger, effect })
    }

    /// 提交只移动已准备账本，不再分配、IO 或返回错误。
    pub fn commit_budget_change(&mut self, prepared: PreparedBudget<T>) {
        *self = prepared.ledger;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn item(name: &'static str, bytes: u64) -> HistoryEntry<&'static str> {
        HistoryEntry::new(name, bytes, Vec::new(), 1)
    }

    #[test]
    fn equal_budget_keeps_only_the_new_entry() {
        let mut h = HistoryLedger::new(64);
        h.commit_prepared(h.prepare(item("a", 32)).unwrap());
        h.commit_prepared(h.prepare(item("b", 64)).unwrap());
        assert_eq!(h.stats().bytes, 64);
        assert_eq!(h.stats().undo_count, 1);
        assert_eq!(h.undo_op(), Some(&"b"));
        assert!(h.prepare(item("too big", 65)).unwrap().requires_fixed_point());
    }

    #[test]
    fn shared_allocation_is_charged_once_until_last_reference_leaves() {
        let shared = Arc::new(String::from("shared"));
        let charge = SharedCharge::of(&shared, 40);
        let mut h = HistoryLedger::new(1_000);
        h.commit_prepared(h.prepare(HistoryEntry::new("a", 10, vec![charge], 1)).unwrap());
        h.commit_prepared(h.prepare(HistoryEntry::new("b", 10, vec![charge], 1)).unwrap());
        assert_eq!(h.stats().bytes, 60);
        let entry = h.take_undo().unwrap();
        h.finish_undo(entry);
        assert_eq!(h.stats().bytes, 60);
        let change = h.set_budget(50);
        assert!(!change.fixed_point);
        assert_eq!(h.stats().undo_count, 0);
        assert_eq!(h.stats().redo_count, 1);
        assert_eq!(h.stats().bytes, 50);
        assert_eq!(h.audited_bytes(), h.stats().bytes);
        h.clear();
        assert_eq!(h.stats().bytes, 0);
        assert_eq!(h.audited_bytes(), 0);
    }

    #[test]
    fn shrinking_evicts_farthest_redo_without_breaking_nearest_chain() {
        let mut h = HistoryLedger::new(1_000);
        for name in ["a", "b", "c"] {
            h.commit_prepared(h.prepare(item(name, 100)).unwrap());
        }
        let entry = h.take_undo().unwrap(); // undo c
        h.finish_undo(entry);
        let entry = h.take_undo().unwrap(); // undo b; redo 顺序为 c,b
        h.finish_undo(entry);
        let change = h.set_budget(150);
        assert!(!change.fixed_point);
        assert_eq!(h.stats().undo_count, 0);
        assert_eq!(h.redo_op(), Some(&"b"));
        assert_eq!(h.stats().bytes, 100);
        assert_eq!(h.audited_bytes(), h.stats().bytes);
        let change = h.set_budget(50);
        assert!(change.fixed_point);
        assert_eq!(h.stats().bytes, 0);
    }


    fn copied_entries<'a>(
        entries: impl Iterator<Item = &'a HistoryEntry<&'static str>>,
    ) -> VecDeque<HistoryEntry<&'static str>> {
        entries.map(|entry| HistoryEntry::new(
            entry.op,
            entry.own_bytes,
            entry.shared.clone(),
            entry.revision_delta,
        )).collect()
    }

    #[test]
    fn rebuild_preserves_order_and_shared_accounting() {
        let shared = Arc::new(String::from("shared"));
        let charge = SharedCharge::of(&shared, 40);
        let mut h = HistoryLedger::new(1_000);
        h.commit_prepared(h.prepare(HistoryEntry::new("a", 10, vec![charge], 1)).unwrap());
        h.commit_prepared(h.prepare(HistoryEntry::new("b", 10, vec![charge], 1)).unwrap());
        h.commit_prepared(h.prepare(item("c", 5)).unwrap());
        let entry = h.take_undo().unwrap();
        h.finish_undo(entry);

        let undo = copied_entries(h.iter_undo());
        let redo = copied_entries(h.iter_redo());
        let prepared = HistoryLedger::prepare_rebuild(undo, redo, 1_000).unwrap();
        h.commit_rebuild(prepared);

        assert_eq!(h.iter_undo().map(|entry| entry.op).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(h.iter_redo().map(|entry| entry.op).collect::<Vec<_>>(), ["c"]);
        assert_eq!(h.stats().bytes, 65);
        assert_eq!(h.audited_bytes(), h.stats().bytes);
    }

    #[test]
    fn rebuild_rejects_invalid_replacement_without_mutating_original() {
        let mut original = HistoryLedger::new(100);
        original.commit_prepared(original.prepare(item("a", 60)).unwrap());
        let replacement = copied_entries(original.iter_undo());
        assert!(matches!(
            HistoryLedger::prepare_rebuild(replacement, VecDeque::new(), 50),
            Err(HistoryError::BudgetExceeded)
        ));
        assert_eq!(original.stats().bytes, 60);
        assert_eq!(original.undo_op(), Some(&"a"));
        assert_eq!(original.audited_bytes(), 60);

        let shared = Arc::new(String::from("shared"));
        let conflict = VecDeque::from([
            HistoryEntry::new("a", 0, vec![SharedCharge::of(&shared, 40)], 1),
            HistoryEntry::new("b", 0, vec![SharedCharge::of(&shared, 41)], 1),
        ]);
        assert!(matches!(
            HistoryLedger::prepare_rebuild(conflict, VecDeque::new(), 100),
            Err(HistoryError::SharedCostConflict)
        ));
    }
    #[test]
    fn cost_accumulation_is_checked() {
        let mut cost = Cost::new();
        cost.add(u64::MAX).unwrap();
        assert_eq!(cost.add(1), Err(HistoryError::CostOverflow));
    }
}

#[cfg(test)]
mod memory_mode_tests {
    use super::*;

    #[test]
    fn resident_budget_uses_ceiling_checked_integer_math() {
        let policy = UndoPolicy { budget_bytes: 64, resident_permille: 1 };
        assert_eq!(effective_undo_budget_bytes(0, false, policy).unwrap(), 64);
        assert_eq!(effective_undo_budget_bytes(1, true, policy).unwrap(), 64);
        assert_eq!(effective_undo_budget_bytes(
            1001,
            true,
            UndoPolicy { budget_bytes: 0, resident_permille: 1 },
        ).unwrap(), 2);
        assert_eq!(effective_undo_budget_bytes(
            1000,
            true,
            UndoPolicy { budget_bytes: 64, resident_permille: 1000 },
        ).unwrap(), 1000);
        assert_eq!(effective_undo_budget_bytes(
            1000,
            true,
            UndoPolicy { budget_bytes: 64, resident_permille: 0 },
        ).unwrap(), 64);
        assert!(effective_undo_budget_bytes(
            u64::MAX,
            true,
            UndoPolicy { budget_bytes: 1, resident_permille: 10_000 },
        ).is_err());
    }

    #[test]
    fn resident_budget_defaults_and_range_are_explicit() {
        let policy = UndoPolicy::default();
        assert_eq!(policy.budget_bytes, DEFAULT_HISTORY_BUDGET_BYTES);
        assert_eq!(policy.resident_permille, DEFAULT_RESIDENT_PERMILLE);
        assert!(effective_undo_budget_bytes(
            1000,
            true,
            UndoPolicy {
                budget_bytes: 64,
                resident_permille: MAX_RESIDENT_PERMILLE + 1,
            },
        ).is_err());
    }

    #[test]
    fn budget_preview_matches_legacy_commit_without_mutating_source() {
        fn ledger_with_redo() -> HistoryLedger<&'static str> {
            let mut ledger = HistoryLedger::new(1_000);
            for name in ["a", "b", "c"] {
                let entry = HistoryEntry::new(name, 100, Vec::new(), 1);
                ledger.commit_prepared(ledger.prepare(entry).unwrap());
            }
            let entry = ledger.take_undo().unwrap();
            ledger.finish_undo(entry);
            let entry = ledger.take_undo().unwrap();
            ledger.finish_undo(entry);
            ledger
        }

        let mut original = ledger_with_redo();
        let before = original.stats();
        let prepared = original.prepare_budget_change(150).unwrap();
        assert_eq!(original.stats(), before, "预检不得改变原账本");
        assert_eq!(prepared.effect.evicted_undo, 1);
        assert_eq!(prepared.effect.evicted_redo, 1);
        assert!(!prepared.effect.establishes_fixed_point);

        let fixed = original.prepare_budget_change(50).unwrap();
        assert!(fixed.effect.establishes_fixed_point);
        assert_eq!(original.stats(), before, "固定点预检同样不得提交");

        let mut expected = ledger_with_redo();
        let legacy = expected.set_budget(150);
        assert_eq!(legacy.evicted, 2);
        assert!(!legacy.fixed_point);
        original.commit_budget_change(prepared);
        assert_eq!(original.stats(), expected.stats());
        assert_eq!(original.redo_op(), Some(&"b"));
        assert_eq!(original.audited_bytes(), original.stats().bytes);
    }
}
