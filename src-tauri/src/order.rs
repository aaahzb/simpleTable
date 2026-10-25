use crate::history::{Cost, HistoryError, SharedCharge};
use std::sync::Arc;

/// 稳定显示排列；Identity 不分配任何排列向量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOrder {
    Identity,
    Perm(Arc<Perm>),
}

/// 显示位置到原始 vrow 的完整排列及其历史持有成本。
#[derive(Debug, PartialEq, Eq)]
pub struct Perm {
    display_to_orig: Vec<u32>,
    history_bytes: u64,
}

impl Perm {
    /// 按结构自身和 Vec 实际容量计算一次共享排列成本。
    pub fn new(display_to_orig: Vec<u32>) -> Result<Self, HistoryError> {
        let mut cost = Cost::new();
        cost.add_value::<Self>()?;
        cost.add_vec(&display_to_orig)?;
        Ok(Self { display_to_orig, history_bytes: cost.bytes() })
    }

    pub fn as_slice(&self) -> &[u32] {
        &self.display_to_orig
    }
}

impl RowOrder {
    /// 当前显示位置映射为原始 vrow；越界回退，交给调用方原子拒绝。
    pub fn display_to_orig(&self, display: u64) -> u64 {
        match self {
            Self::Identity => display,
            Self::Perm(perm) => usize::try_from(display)
                .ok()
                .and_then(|display| perm.display_to_orig.get(display))
                .copied()
                .map_or(display, u64::from),
        }
    }

    pub fn history_bytes(&self) -> u64 {
        match self {
            Self::Identity => 0,
            Self::Perm(perm) => perm.history_bytes,
        }
    }

    /// 排列按历史 entry 引用计费，而不是按 Arc clone 次数计费。
    pub fn shared_charge(&self) -> Option<SharedCharge> {
        match self {
            Self::Identity => None,
            Self::Perm(perm) => Some(SharedCharge::of(perm, self.history_bytes())),
        }
    }
}

/// 将稳定原始行命中投影为当前显示命中，并保持显示位置升序。
pub fn project_filter(stable_hits: &[u32], order: &RowOrder) -> Vec<u32> {
    let RowOrder::Perm(perm) = order else {
        return stable_hits.to_vec();
    };
    if stable_hits.is_empty() {
        return Vec::new();
    }
    // rank 只在投影时临时存在；历史与活动状态共享的是 Perm，不共享 rank。
    let mut rank = vec![u32::MAX; perm.display_to_orig.len()];
    for (display, &orig) in perm.display_to_orig.iter().enumerate() {
        let Ok(display) = u32::try_from(display) else { continue };
        if let Some(slot) = rank.get_mut(orig as usize) {
            *slot = display;
        }
    }
    let mut projected: Vec<u32> = stable_hits
        .iter()
        .filter_map(|&orig| rank.get(orig as usize).copied())
        .filter(|&display| display != u32::MAX)
        .collect();
    projected.sort_unstable();
    projected
}

#[cfg(test)]
mod tests {
    use super::{project_filter, Perm, RowOrder};
    use std::sync::Arc;

    #[test]
    fn permutation_cost_and_safe_projection_cover_full_vector() {
        let perm = Perm::new(vec![2, 0, 1]).unwrap();
        let order = RowOrder::Perm(Arc::new(perm));
        let expected = (std::mem::size_of::<Perm>() + 3 * std::mem::size_of::<u32>()) as u64;
        assert_eq!(order.history_bytes(), expected);
        assert_eq!(order.display_to_orig(0), 2);
        assert_eq!(order.display_to_orig(2), 1);
        assert_eq!(order.display_to_orig(99), 99, "越界必须回退并由调用方拒绝");
        assert_eq!(project_filter(&[0, 2, 99], &order), vec![0, 1]);
        assert!(order.shared_charge().is_some());
    }

    #[test]
    fn identity_order_has_no_permutation_allocation() {
        let identity = RowOrder::Identity;
        assert_eq!(identity.history_bytes(), 0);
        assert_eq!(identity.display_to_orig(123), 123);
        assert!(identity.shared_charge().is_none());
    }
}
