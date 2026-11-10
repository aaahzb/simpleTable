//! 真实进程内存采样：私有字节用于硬断言，其余指标只作诊断。

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct ProcessMemorySample {
    pub private_bytes: u64,
    pub working_set_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SystemMemory {
    pub available_physical_bytes: u64,
    pub available_commit_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct AnonymousRegionAudit {
    pub matched_private_bytes: u64,
    pub region_count: u64,
}

/// 采样当前进程；None 与 Some(当前 PID) 分别覆盖内外部入口。
pub fn sample_current_process() -> Result<ProcessMemorySample, String> {
    sample_process(None)
}

#[cfg(windows)]
pub fn sample_process(pid: Option<u32>) -> Result<ProcessMemorySample, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let (handle, owned) = match pid {
        Some(pid) => {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if handle.is_null() {
                return Err(format!(
                    "OpenProcess({pid}) 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            (handle, true)
        }
        None => unsafe { (GetCurrentProcess(), false) },
    };
    let result = (|| {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = unsafe {
            GetProcessMemoryInfo(
                handle,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                counters.cb,
            )
        };
        if ok == 0 {
            return Err(format!(
                "GetProcessMemoryInfo 失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ProcessMemorySample {
            private_bytes: counters.PrivateUsage as u64,
            working_set_bytes: Some(counters.WorkingSetSize as u64),
        })
    })();
    if owned {
        unsafe { CloseHandle(handle) };
    }
    result
}

#[cfg(windows)]
pub fn available_system_memory() -> Result<SystemMemory, String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(format!(
            "GlobalMemoryStatusEx 失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(SystemMemory {
        available_physical_bytes: status.ullAvailPhys,
        available_commit_bytes: Some(status.ullAvailPageFile),
    })
}

#[cfg(target_os = "macos")]
mod os {
    pub type KernReturn = i32;
    pub type MachPort = u32;
    pub const KERN_SUCCESS: KernReturn = 0;
    pub const KERN_INVALID_ADDRESS: KernReturn = 1;
    pub const TASK_VM_INFO: u32 = 22;
    pub const VM_REGION_EXTENDED_INFO: i32 = 13;
    pub const HOST_VM_INFO64: u32 = 4;
    pub const SM_COW: u8 = 1;
    pub const SM_PRIVATE: u8 = 2;
    pub const SM_PRIVATE_ALIASED: u8 = 6;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct TaskVmInfo {
        pub virtual_size: u64,
        pub region_count: i32,
        pub page_size: i32,
        pub resident_size: u64,
        pub resident_size_peak: u64,
        pub device: u64,
        pub device_peak: u64,
        pub internal: u64,
        pub internal_peak: u64,
        pub external: u64,
        pub external_peak: u64,
        pub reusable: u64,
        pub reusable_peak: u64,
        pub purgeable_volatile_pmap: u64,
        pub purgeable_volatile_resident: u64,
        pub purgeable_volatile_virtual: u64,
        pub compressed: u64,
        pub compressed_peak: u64,
        pub compressed_lifetime: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmRegionExtendedInfo {
        pub protection: i32,
        pub user_tag: u32,
        pub pages_resident: u32,
        pub pages_shared_now_private: u32,
        pub pages_swapped_out: u32,
        pub pages_dirtied: u32,
        pub ref_count: u32,
        pub shadow_depth: u16,
        pub external_pager: u8,
        pub share_mode: u8,
        pub pages_reusable: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmStatistics64 {
        pub free_count: u32,
        pub active_count: u32,
        pub inactive_count: u32,
        pub wire_count: u32,
        pub zero_fill_count: u64,
        pub reactivations: u64,
        pub pageins: u64,
        pub pageouts: u64,
        pub faults: u64,
        pub cow_faults: u64,
        pub lookups: u64,
        pub hits: u64,
        pub purges: u64,
        pub purgeable_count: u32,
        pub speculative_count: u32,
        pub decompressions: u64,
        pub compressions: u64,
        pub swapins: u64,
        pub swapouts: u64,
        pub compressor_page_count: u32,
        pub throttled_count: u32,
        pub external_page_count: u32,
        pub internal_page_count: u32,
        pub total_uncompressed_pages_in_compressor: u64,
    }

    extern "C" {
        pub static mach_task_self_: MachPort;
        pub fn mach_host_self() -> MachPort;
        pub fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        pub fn task_for_pid(task: MachPort, pid: i32, target: *mut MachPort) -> KernReturn;
        pub fn task_info(task: MachPort, flavor: u32, out: *mut i32, count: *mut u32)
            -> KernReturn;
        pub fn mach_vm_region(
            task: MachPort,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut i32,
            count: *mut u32,
            object_name: *mut MachPort,
        ) -> KernReturn;
        pub fn host_statistics64(
            host: MachPort,
            flavor: u32,
            out: *mut i32,
            count: *mut u32,
        ) -> KernReturn;
    }

    pub unsafe fn mach_task_self() -> MachPort {
        mach_task_self_
    }
}

#[cfg(target_os = "macos")]
use os::*;

#[cfg(target_os = "macos")]
fn sample_task(task: MachPort) -> Result<ProcessMemorySample, String> {
    let mut info = TaskVmInfo::default();
    let expected_count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
    let mut count = expected_count;
    let kr = unsafe {
        task_info(
            task,
            TASK_VM_INFO,
            (&mut info as *mut TaskVmInfo).cast(),
            &mut count,
        )
    };
    if kr != KERN_SUCCESS {
        return Err(format!("task_info(TASK_VM_INFO) 失败：{kr}"));
    }
    if count != expected_count {
        return Err(format!(
            "task_info(TASK_VM_INFO) 计数错误：期望 {expected_count}，实际 {count}"
        ));
    }
    let private = info
        .internal
        .checked_add(info.compressed)
        .ok_or_else(|| "macOS 私有字节相加溢出".to_string())?;
    Ok(ProcessMemorySample {
        private_bytes: private,
        working_set_bytes: Some(info.resident_size),
    })
}

#[cfg(target_os = "macos")]
pub fn sample_process(pid: Option<u32>) -> Result<ProcessMemorySample, String> {
    let self_task = unsafe { mach_task_self() };
    let Some(pid) = pid else {
        return sample_task(self_task);
    };
    let pid = i32::try_from(pid).map_err(|_| "PID 超出 macOS i32 范围".to_string())?;
    let mut task = 0;
    let kr = unsafe { task_for_pid(self_task, pid, &mut task) };
    if kr != KERN_SUCCESS {
        return Err(format!("task_for_pid({pid}) 失败：{kr}；禁止降级为工作集"));
    }
    let result = sample_task(task);
    unsafe { mach_port_deallocate(self_task, task) };
    result
}

#[cfg(target_os = "macos")]
pub fn available_system_memory() -> Result<SystemMemory, String> {
    let mut info = VmStatistics64::default();
    let expected_count = (std::mem::size_of::<VmStatistics64>() / 4) as u32;
    let mut count = expected_count;
    let kr = unsafe {
        host_statistics64(
            mach_host_self(),
            HOST_VM_INFO64,
            (&mut info as *mut VmStatistics64).cast(),
            &mut count,
        )
    };
    if kr != KERN_SUCCESS {
        return Err(format!("host_statistics64(HOST_VM_INFO64) 失败：{kr}"));
    }
    if count != expected_count {
        return Err(format!(
            "host_statistics64 计数错误：期望 {expected_count}，实际 {count}"
        ));
    }
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err(format!("sysconf(_SC_PAGESIZE) 失败：{page_size}"));
    }
    let pages = (info.free_count as u64)
        .checked_add(info.inactive_count as u64)
        .and_then(|value| value.checked_add(info.speculative_count as u64))
        .and_then(|value| value.checked_add(info.purgeable_count as u64))
        .ok_or_else(|| "macOS 可用页数相加溢出".to_string())?;
    let available = pages
        .checked_mul(page_size as u64)
        .ok_or_else(|| "macOS 可用物理字节计算溢出".to_string())?;
    Ok(SystemMemory {
        available_physical_bytes: available,
        available_commit_bytes: None,
    })
}

/// 审计匿名私有区域的虚拟交集；该值只证明身份与释放，不代表物理 commit。
#[cfg(target_os = "macos")]
pub fn audit_task_regions(
    task: MachPort,
    range: Option<(u64, u64)>,
) -> Result<AnonymousRegionAudit, String> {
    let mut address = range.map_or(0, |(base, _)| base);
    let end = match range {
        Some((base, len)) => base
            .checked_add(len)
            .ok_or_else(|| "区域审计范围溢出".to_string())?,
        None => u64::MAX,
    };
    let mut audit = AnonymousRegionAudit::default();
    while address < end {
        let mut region_base = address;
        let mut region_size = 0u64;
        let mut info = VmRegionExtendedInfo::default();
        let expected_count = (std::mem::size_of::<VmRegionExtendedInfo>() / 4) as u32;
        let mut count = expected_count;
        let mut object_name = 0;
        let kr = unsafe {
            mach_vm_region(
                task,
                &mut region_base,
                &mut region_size,
                VM_REGION_EXTENDED_INFO,
                (&mut info as *mut VmRegionExtendedInfo).cast(),
                &mut count,
                &mut object_name,
            )
        };
        if kr == KERN_INVALID_ADDRESS {
            break;
        }
        if kr != KERN_SUCCESS {
            return Err(format!("mach_vm_region({address:#x}) 失败：{kr}"));
        }
        if object_name != 0 {
            unsafe { mach_port_deallocate(mach_task_self(), object_name) };
        }
        if count != expected_count {
            return Err(format!(
                "mach_vm_region 计数错误：期望 {expected_count}，实际 {count}"
            ));
        }
        if region_size == 0 {
            return Err(format!("mach_vm_region({address:#x}) 返回零长度区域"));
        }
        let region_end = region_base
            .checked_add(region_size)
            .ok_or_else(|| "区域地址计算溢出".to_string())?;
        if region_end <= address {
            return Err(format!("mach_vm_region({address:#x}) 未向前推进"));
        }
        if region_base >= end {
            break;
        }
        let anonymous_private = info.external_pager == 0
            && matches!(info.share_mode, SM_COW | SM_PRIVATE | SM_PRIVATE_ALIASED);
        if anonymous_private {
            let intersection_start = region_base.max(address);
            let intersection_end = region_end.min(end);
            if intersection_end > intersection_start {
                let bytes = intersection_end - intersection_start;
                audit.matched_private_bytes = audit
                    .matched_private_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| "匿名私有区域字节累计溢出".to_string())?;
                audit.region_count = audit
                    .region_count
                    .checked_add(1)
                    .ok_or_else(|| "匿名私有区域计数溢出".to_string())?;
            }
        }
        address = region_end;
    }
    Ok(audit)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn sample_process(_pid: Option<u32>) -> Result<ProcessMemorySample, String> {
    Err("当前平台没有实现真实私有内存采样".into())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn available_system_memory() -> Result<SystemMemory, String> {
    Err("当前平台没有实现系统内存采样".into())
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    use super::{available_system_memory, sample_current_process, sample_process};

    #[test]
    fn current_process_reports_real_private_and_available_memory() {
        let sample = sample_current_process().unwrap();
        assert!(sample.private_bytes > 0);
        assert!(sample.working_set_bytes.unwrap_or(1) > 0);
        let system = available_system_memory().unwrap();
        assert!(system.available_physical_bytes > 0);
        #[cfg(windows)]
        assert!(system.available_commit_bytes.unwrap() > 0);
        #[cfg(target_os = "macos")]
        assert!(system.available_commit_bytes.is_none());
    }

    #[test]
    fn explicit_pid_uses_the_external_process_sampling_entry() {
        let sample = sample_process(Some(std::process::id())).unwrap();
        assert!(sample.private_bytes > 0);
        assert!(sample.working_set_bytes.unwrap_or(1) > 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_task_vm_info_abi_count_and_call_are_valid() {
        use super::os::{mach_task_self, task_info, TaskVmInfo, KERN_SUCCESS, TASK_VM_INFO};

        let mut info = TaskVmInfo::default();
        let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
        assert_eq!(std::mem::size_of::<TaskVmInfo>(), 144);
        assert_eq!(count, 36);
        let kr = unsafe {
            task_info(
                mach_task_self(),
                TASK_VM_INFO,
                (&mut info as *mut TaskVmInfo).cast(),
                &mut count,
            )
        };
        assert_eq!(kr, KERN_SUCCESS);
        assert_eq!(count, 36);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_anonymous_region_audit_covers_ram_and_detects_release() {
        use super::{audit_task_regions, os::mach_task_self};
        use crate::ram_region::RamRegion;

        let mut region = RamRegion::reserve(16 * 1024 * 1024).unwrap();
        region
            .for_each_chunk_mut(
                1024 * 1024,
                2,
                |_| {},
                |_, out| {
                    out.fill(0x5a);
                    Ok(())
                },
            )
            .unwrap();
        let ram = region.finish().unwrap();
        let base = ram.as_ptr() as u64;
        let audit = audit_task_regions(unsafe { mach_task_self() }, Some((base, ram.len() as u64)))
            .unwrap();
        assert!(audit.matched_private_bytes >= ram.len() as u64);
        assert!(audit.region_count > 0);
        drop(ram);
        let after = audit_task_regions(unsafe { mach_task_self() }, Some((base, 16 * 1024 * 1024)));
        assert!(after.is_err() || after.unwrap().matched_private_bytes == 0);
    }
}
