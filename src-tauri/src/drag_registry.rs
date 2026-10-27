//! 原生鼠标坐标命中与跨窗口标签拖放登记。
//!
//! 前端只持有 dragToken；Pointer Capture 客户端坐标与源窗口几何换算成真实落点，
//! 目标窗口通过一次性通道回复标签组和插入索引。取消、超时及窗口销毁都会解除等待。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::oneshot;

/// 原生物理像素坐标，允许负数以支持主显示器左侧或上方的显示器。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

/// Pointer Capture 提供的源 WebView 客户端 CSS 坐标，移出窗口后允许为负或超出视口。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClientPoint {
    pub x: f64,
    pub y: f64,
}

/// 用源窗口内框物理原点与缩放换算桌面坐标，不依赖 RDP 下可能失败的 GetCursorPos。
pub fn client_to_physical(
    origin: PhysicalPoint,
    scale: f64,
    client: ClientPoint,
) -> Result<PhysicalPoint, String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("源窗口 scale factor 无效".into());
    }
    if !client.x.is_finite() || !client.y.is_finite() {
        return Err("客户端拖放坐标不是有限数值".into());
    }
    let x = f64::from(origin.x) + client.x * scale;
    let y = f64::from(origin.y) + client.y * scale;
    if !x.is_finite() || !y.is_finite()
        || x < i32::MIN as f64 || x > i32::MAX as f64
        || y < i32::MIN as f64 || y > i32::MAX as f64
    {
        return Err("客户端拖放坐标超出物理坐标范围".into());
    }
    Ok(PhysicalPoint { x: x.round() as i32, y: y.round() as i32 })
}

/// 一个已就绪且可见的 simpleTable 窗口外框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRect {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl WindowRect {
    fn contains(&self, point: PhysicalPoint) -> bool {
        let x = i64::from(point.x);
        let y = i64::from(point.y);
        let left = i64::from(self.x);
        let top = i64::from(self.y);
        x >= left
            && y >= top
            && x < left + i64::from(self.w)
            && y < top + i64::from(self.h)
    }
}

/// 按调用方给出的优先级命中窗口；右缘和下缘不属于窗口。
pub fn hit_test(windows: &[WindowRect], point: PhysicalPoint) -> Option<String> {
    windows.iter().find(|window| window.contains(point)).map(|window| window.label.clone())
}

/// 原生落点的三类路由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropRoute {
    SameWindow { x: i32, y: i32 },
    AskTarget { label: String, x: i32, y: i32 },
    NewWindow { x: i32, y: i32 },
}

/// 将原生落点映射到源窗口、已有目标或桌面新窗口。
pub fn route_drop(windows: &[WindowRect], source: &str, point: PhysicalPoint) -> DropRoute {
    match hit_test(windows, point) {
        Some(label) if label == source => DropRoute::SameWindow { x: point.x, y: point.y },
        Some(label) => DropRoute::AskTarget { label, x: point.x, y: point.y },
        None => DropRoute::NewWindow { x: point.x, y: point.y },
    }
}

/// 目标窗口确认的有效插入位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropTarget {
    pub side: String,
    pub index: usize,
}

/// hover 只需让源窗口绘制本窗指示器；其他窗口由定向事件自行绘制。
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DragHover {
    SameWindow { x: f64, y: f64 },
    Existing,
    NewWindow,
    Invalid,
}

/// drop 的最终路由；Existing 已包含目标确认的分组和索引。
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DropOutcome {
    SameWindow { x: f64, y: f64 },
    Existing { target: String, side: String, index: usize },
    NewWindow { x: i32, y: i32 },
    Invalid,
}

struct ActiveDrag {
    token: u64,
    session: u64,
    source: String,
    lease: u64,
    hover: Option<String>,
}

struct PendingDrop {
    token: u64,
    target: String,
    sender: oneshot::Sender<Option<DropTarget>>,
}

/// 进程内最多一个鼠标拖放；pending drop 可跨 async 等待而不持运行态锁。
#[derive(Default)]
pub struct DragRegistry {
    active: Option<ActiveDrag>,
    pending: HashMap<u64, PendingDrop>,
    next_drag: u64,
    next_drop: u64,
}

impl DragRegistry {
    /// 登记一次拖放；文档 owner/lease 由调用方在 DocumentRegistry 中校验。
    pub fn begin(&mut self, session: u64, source: &str, lease: u64) -> Result<u64, String> {
        if self.active.is_some() {
            return Err("已有标签拖放尚未结束".into());
        }
        self.next_drag = self.next_drag.checked_add(1).expect("拖放 token 已耗尽");
        let token = self.next_drag;
        self.active = Some(ActiveDrag {
            token,
            session,
            source: source.to_string(),
            lease,
            hover: None,
        });
        Ok(token)
    }

    /// 校验命令确实来自拖起源窗口，并返回文档授权参数。
    pub fn authorize(&self, token: u64, source: &str) -> Result<(u64, u64), String> {
        let active = self.active.as_ref().ok_or_else(|| "标签拖放已结束".to_string())?;
        if active.token != token || active.source != source {
            return Err("拖放 token 或源窗口不匹配".into());
        }
        Ok((active.session, active.lease))
    }

    /// 记录当前 hover 目标，返回需要清除指示器的旧目标。
    pub fn set_hover(
        &mut self,
        token: u64,
        source: &str,
        target: Option<String>,
    ) -> Result<Option<String>, String> {
        self.authorize(token, source)?;
        let active = self.active.as_mut().expect("authorize 后拖放必须存在");
        if active.hover == target {
            return Ok(None);
        }
        let previous = active.hover.clone();
        active.hover = target;
        Ok(previous)
    }

    /// 为已有目标建立一次性回执通道。
    pub fn ask_target(
        &mut self,
        token: u64,
        source: &str,
        target: &str,
    ) -> Result<(u64, oneshot::Receiver<Option<DropTarget>>), String> {
        self.authorize(token, source)?;
        if self.pending.values().any(|pending| pending.token == token) {
            return Err("同一拖放已有等待中的投放".into());
        }
        self.next_drop = self.next_drop.checked_add(1).expect("dropId 已耗尽");
        let drop_id = self.next_drop;
        let (sender, receiver) = oneshot::channel();
        self.pending.insert(drop_id, PendingDrop {
            token,
            target: target.to_string(),
            sender,
        });
        Ok((drop_id, receiver))
    }

    /// 目标窗口回复有效位置或无效区域；错误窗口不能消费 dropId。
    pub fn drop_result(
        &mut self,
        drop_id: u64,
        target: &str,
        side: Option<&str>,
        index: Option<usize>,
    ) -> Result<(), String> {
        let reply = match (side, index) {
            (Some(side @ ("A" | "B")), Some(index)) => Some(DropTarget {
                side: side.to_string(), index,
            }),
            (None, None) => None,
            _ => return Err("投放回执必须同时提供有效 side/index，或两者均为空".into()),
        };
        let pending = self.pending.get(&drop_id).ok_or_else(|| "投放已结束或 dropId 无效".to_string())?;
        if pending.target != target {
            return Err("窗口不是该投放的目标".into());
        }
        let pending = self.pending.remove(&drop_id).expect("已校验的投放必须存在");
        pending.sender.send(reply).map_err(|_| "投放等待方已退出".into())
    }

    /// 超时或事件发送失败后移除回执，迟到回复因此无效。
    pub fn abort_drop(&mut self, drop_id: u64) -> bool {
        self.pending.remove(&drop_id).is_some()
    }

    fn cancel_pending(&mut self, token: u64) {
        let ids = self.pending.iter()
            .filter(|(_, pending)| pending.token == token)
            .map(|(drop_id, _)| *drop_id)
            .collect::<Vec<_>>();
        for drop_id in ids {
            if let Some(pending) = self.pending.remove(&drop_id) {
                let _ = pending.sender.send(None);
            }
        }
    }

    fn finish_inner(&mut self, token: u64, source: &str) -> Result<Option<String>, String> {
        if self.active.is_none() {
            return Ok(None);
        }
        self.authorize(token, source)?;
        let active = self.active.take().expect("authorize 后拖放必须存在");
        self.cancel_pending(token);
        Ok(active.hover)
    }

    /// 正常结束拖放，并返回最后需要清理 hover 的目标。
    pub fn finish(&mut self, token: u64, source: &str) -> Result<Option<String>, String> {
        self.finish_inner(token, source)
    }

    /// Escape/pointercancel：解除所有等待，不产出任何迁移路由。
    pub fn cancel(&mut self, token: u64, source: &str) -> Result<Option<String>, String> {
        self.finish_inner(token, source)
    }

    /// 源销毁取消整次拖放并请求回收预览；目标销毁只解除等待中的 drop。
    pub fn window_destroyed(&mut self, label: &str) -> bool {
        let source_token = self.active.as_ref()
            .filter(|active| active.source == label)
            .map(|active| active.token);
        if let Some(token) = source_token {
            self.active = None;
            self.cancel_pending(token);
        }
        if let Some(active) = self.active.as_mut() {
            if active.hover.as_deref() == Some(label) {
                active.hover = None;
            }
        }
        let ids = self.pending.iter()
            .filter(|(_, pending)| pending.target == label)
            .map(|(drop_id, _)| *drop_id)
            .collect::<Vec<_>>();
        for drop_id in ids {
            if let Some(pending) = self.pending.remove(&drop_id) {
                let _ = pending.sender.send(None);
            }
        }
        source_token.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{client_to_physical, hit_test, route_drop, ClientPoint, DragRegistry, DropRoute, DropTarget, PhysicalPoint, WindowRect};

    #[test]
    fn rdp_client_points_become_physical_without_global_cursor() {
        let origin = PhysicalPoint { x: -1200, y: 300 };
        assert_eq!(
            client_to_physical(origin, 1.5, ClientPoint { x: 40.0, y: -80.0 }),
            Ok(PhysicalPoint { x: -1140, y: 180 }),
        );
        assert_eq!(
            client_to_physical(
                PhysicalPoint { x: 100, y: 200 },
                1.25,
                ClientPoint { x: -200.0, y: 50.0 },
            ),
            Ok(PhysicalPoint { x: -150, y: 263 }),
        );
    }

    #[test]
    fn invalid_client_geometry_is_rejected_before_hit_testing() {
        let origin = PhysicalPoint { x: 0, y: 0 };
        assert!(client_to_physical(origin, 0.0, ClientPoint { x: 1.0, y: 1.0 }).is_err());
        assert!(client_to_physical(origin, f64::NAN, ClientPoint { x: 1.0, y: 1.0 }).is_err());
        assert!(client_to_physical(origin, 1.0, ClientPoint { x: f64::INFINITY, y: 1.0 }).is_err());
        assert!(client_to_physical(origin, 1.0, ClientPoint { x: f64::MAX, y: 1.0 }).is_err());
    }

    #[test]
    fn physical_hit_testing_supports_negative_monitor_coordinates() {
        let windows = vec![WindowRect {
            label: "main".into(), x: -1200, y: 0, w: 1000, h: 800,
        }];
        assert_eq!(
            hit_test(&windows, PhysicalPoint { x: -300, y: 200 }).as_deref(),
            Some("main"),
        );
        assert_eq!(hit_test(&windows, PhysicalPoint { x: 50, y: 200 }), None);
    }

    #[test]
    fn window_edges_include_top_left_and_exclude_bottom_right() {
        let windows = vec![WindowRect {
            label: "doc-1".into(), x: 100, y: 200, w: 800, h: 600,
        }];
        assert_eq!(hit_test(&windows, PhysicalPoint { x: 100, y: 200 }).as_deref(), Some("doc-1"));
        assert_eq!(hit_test(&windows, PhysicalPoint { x: 899, y: 799 }).as_deref(), Some("doc-1"));
        assert_eq!(hit_test(&windows, PhysicalPoint { x: 900, y: 799 }), None);
        assert_eq!(hit_test(&windows, PhysicalPoint { x: 899, y: 800 }), None);
    }

    #[test]
    fn drop_routes_same_window_existing_target_and_desktop() {
        let windows = vec![
            WindowRect { label: "main".into(), x: 0, y: 0, w: 800, h: 600 },
            WindowRect { label: "doc-1".into(), x: 900, y: 0, w: 800, h: 600 },
        ];
        assert_eq!(
            route_drop(&windows, "main", PhysicalPoint { x: 400, y: 300 }),
            DropRoute::SameWindow { x: 400, y: 300 },
        );
        assert_eq!(
            route_drop(&windows, "main", PhysicalPoint { x: 1000, y: 300 }),
            DropRoute::AskTarget { label: "doc-1".into(), x: 1000, y: 300 },
        );
        assert_eq!(
            route_drop(&windows, "main", PhysicalPoint { x: 1800, y: 300 }),
            DropRoute::NewWindow { x: 1800, y: 300 },
        );
    }

    #[test]
    fn existing_target_replies_with_group_and_insert_index() {
        let mut registry = DragRegistry::default();
        let token = registry.begin(9, "main", 3).unwrap();
        let (drop_id, mut receiver) = registry.ask_target(token, "main", "doc-1").unwrap();
        assert_eq!(registry.drop_result(drop_id, "doc-1", Some("B"), Some(2)), Ok(()));
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(DropTarget { side: "B".into(), index: 2 }),
        );
    }

    #[test]
    fn invalid_target_and_wrong_window_cannot_forge_a_drop() {
        let mut registry = DragRegistry::default();
        let token = registry.begin(9, "main", 3).unwrap();
        let (drop_id, mut receiver) = registry.ask_target(token, "main", "doc-1").unwrap();
        assert!(registry.drop_result(drop_id, "main", Some("A"), Some(0)).is_err());
        assert!(receiver.try_recv().is_err());
        assert_eq!(registry.drop_result(drop_id, "doc-1", None, None), Ok(()));
        assert_eq!(receiver.try_recv().unwrap(), None);
        assert!(registry.drop_result(drop_id, "doc-1", Some("A"), Some(0)).is_err());
    }

    #[test]
    fn cancel_and_target_destroy_resolve_pending_drop_without_migration() {
        let mut registry = DragRegistry::default();
        let token = registry.begin(9, "main", 3).unwrap();
        let (_, mut cancelled) = registry.ask_target(token, "main", "doc-1").unwrap();
        registry.cancel(token, "main").unwrap();
        assert_eq!(cancelled.try_recv().unwrap(), None);

        let token = registry.begin(9, "main", 3).unwrap();
        let (_, mut destroyed) = registry.ask_target(token, "main", "doc-1").unwrap();
        registry.window_destroyed("doc-1");
        assert_eq!(destroyed.try_recv().unwrap(), None);
    }

    #[test]
    fn source_destroy_requests_preview_cleanup_but_target_destroy_does_not() {
        let mut source = DragRegistry::default();
        let token = source.begin(9, "main", 3).unwrap();
        assert!(source.window_destroyed("main"));
        assert!(source.authorize(token, "main").is_err());

        let mut target = DragRegistry::default();
        let token = target.begin(9, "main", 3).unwrap();
        assert!(!target.window_destroyed("doc-1"));
        assert!(target.authorize(token, "main").is_ok());
    }

    #[test]
    fn timeout_cleanup_rejects_a_late_target_reply() {
        let mut registry = DragRegistry::default();
        let token = registry.begin(9, "main", 3).unwrap();
        let (drop_id, _receiver) = registry.ask_target(token, "main", "doc-1").unwrap();
        assert!(registry.abort_drop(drop_id));
        assert!(!registry.abort_drop(drop_id));
        assert!(registry.drop_result(drop_id, "doc-1", Some("A"), Some(0)).is_err());
    }
}
