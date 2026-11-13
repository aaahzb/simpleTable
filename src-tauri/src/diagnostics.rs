use crate::engine::{BigFile, ColOp, MEMORY_MODE_MIN_BYTES};
use crate::history::Approval;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub use crate::engine::{SaveRamEvent, SaveRamPhase};
pub use crate::process_memory::{sample_current_process, sample_process, ProcessMemorySample};
pub use crate::ram_load::{LoadEvent, LoadFault, LoadOptions};

/// 独立内存诊断使用的真实文档；所有操作都直接调用生产引擎。
#[doc(hidden)]
pub struct DiagnosticDocument {
    inner: BigFile,
}

impl DiagnosticDocument {
    /// 同步完成真实打开和索引，不经过 cfg(test) 构造器。
    pub fn open_complete(path: &Path) -> Result<Self, String> {
        Ok(Self {
            inner: BigFile::open_complete(path, false)?,
        })
    }

    /// 在 READY 前顺序触碰真实后备字节，预热 optional preload 矩阵。
    pub fn preload_source(&self) {
        let checksum = self
            .inner
            .bytes()
            .iter()
            .fold(0u64, |sum, byte| sum.wrapping_add(*byte as u64));
        std::hint::black_box(checksum);
    }

    /// 使用生产转换入口，保留全部真实分配、读取、取消和恢复行为。
    pub fn enter_memory_mode<O>(
        &mut self,
        cancel: &AtomicBool,
        options: LoadOptions,
        observer: O,
    ) -> Result<bool, String>
    where
        O: Fn(LoadEvent) + Sync,
    {
        self.inner
            .enter_memory_mode_with_options(cancel, MEMORY_MODE_MIN_BYTES, options, observer)
    }

    /// 返回进程内仍存续的诊断区域虚拟预留字节，仅用于泄漏检查。
    pub fn live_reserved_bytes() -> usize {
        crate::ram_region::live_reserved_bytes()
    }

    /// 返回当前真实后备文件字节数。
    pub fn backing_file_bytes(&self) -> u64 {
        self.inner.meta().backing_file_bytes
    }

    /// 通过生产编辑入口修改一个单元格。
    pub fn set_cell(&mut self, row: u64, col: u32, value: String) -> Result<(), String> {
        self.inner
            .set_cell(
                row,
                col,
                value,
                Approval {
                    allow_fixed_point: false,
                    preflight_id: None,
                },
            )
            .map(|_| ())
    }

    /// 通过生产列操作入口插入一个空列并提交 Schema。
    pub fn insert_column(&mut self, pos: u32, name: String) -> Result<(), String> {
        self.insert_column_with_ram_observer(pos, name, |_| {})
    }

    /// 插入空列并转发列操作期间真实的 RAM 物化阶段。
    pub fn insert_column_with_ram_observer<O>(
        &mut self,
        pos: u32,
        name: String,
        observer: O,
    ) -> Result<(), String>
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        self.inner.push_col_op(ColOp::Insert { pos, name })?;
        let outcome = self.inner.apply_col_ops_observed(
            Approval {
                allow_fixed_point: false,
                preflight_id: None,
            },
            &observer,
        )?;
        if outcome.changed {
            Ok(())
        } else {
            Err("真实插列未产生变化".into())
        }
    }

    /// 通过生产保存入口写盘，并转发全部真实 RAM 与磁盘提交阶段。
    ///
    /// 诊断目标与源文件不同，必须使用另存为语义（`switch_path = true`）：
    /// 否则会话保存点会指向目标文件而 `path` 仍是源文件，第二次保存会被
    /// `stale()` 误判为外部变化，无法验证同一 RAM 的重试路径。
    pub fn save_with_ram_observer<O>(
        &mut self,
        dest: &Path,
        options: LoadOptions,
        observer: O,
    ) -> Result<(), String>
    where
        O: Fn(SaveRamEvent) + Sync,
    {
        self.inner
            .save_to_observed(
                dest,
                true,
                Approval {
                    allow_fixed_point: false,
                    preflight_id: None,
                },
                options,
                |_, _, _, _| {},
                observer,
            )
            .map(|_| ())
    }

    /// 读取真实视图行，供诊断验证文档仍可使用。
    pub fn rows(&self, from: u64, count: u32) -> Vec<Vec<String>> {
        self.inner.rows(from, count)
    }
}
