use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let code = match run_cli(std::env::args().skip(1)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ERROR {error}");
            2
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

fn run_cli<I>(args: I) -> Result<i32, String>
where
    I: IntoIterator<Item = String>,
{
    match parse_args(args)? {
        CliCommand::Run { config, report } => {
            let report = run_parent(&config, &report)?;
            Ok(report_exit_code(&report))
        }
        CliCommand::Child { config } => {
            run_child_stdio(&config)?;
            Ok(0)
        }
    }
}

fn run_parent(config: &ChildConfig, report_path: &Path) -> Result<PeakReport, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("定位当前 memory_peak 失败：{error}"))?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("child")
        .arg("--scenario")
        .arg(config.scenario.as_str())
        .arg("--file")
        .arg(&config.file)
        .arg("--workers")
        .arg(config.workers.to_string())
        .arg("--preload")
        .arg(config.preload.to_string());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    run_parent_with_command(config, report_path, &mut command, &mut output)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Convert,
    Cancel,
    CommitFail,
    ReadFail,
    Save,
    ColumnApply,
    ColumnSave,
    SaveRetry,
    CancelLoop,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "convert" => Ok(Self::Convert),
            "cancel" => Ok(Self::Cancel),
            "commit-fail" => Ok(Self::CommitFail),
            "read-fail" => Ok(Self::ReadFail),
            "save" => Ok(Self::Save),
            "column-apply" => Ok(Self::ColumnApply),
            "column-save" => Ok(Self::ColumnSave),
            "save-retry" => Ok(Self::SaveRetry),
            "cancel-loop" => Ok(Self::CancelLoop),
            _ => Err(format!("未知场景：{value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Convert => "convert",
            Self::Cancel => "cancel",
            Self::CommitFail => "commit-fail",
            Self::ReadFail => "read-fail",
            Self::Save => "save",
            Self::ColumnApply => "column-apply",
            Self::ColumnSave => "column-save",
            Self::SaveRetry => "save-retry",
            Self::CancelLoop => "cancel-loop",
        }
    }
}

/// 场景是否属于持久内存矩阵：READY 前必须先进入内存模式。
fn requires_memory_mode(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::Save | Scenario::ColumnApply | Scenario::ColumnSave | Scenario::SaveRetry
    )
}

/// 场景是否必须产生真实磁盘写盘与提交事件；纯列操作只整理 RAM。
fn requires_disk_save(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::Save | Scenario::ColumnSave | Scenario::SaveRetry
    )
}

#[derive(Clone, Debug)]
struct ChildConfig {
    scenario: Scenario,
    file: PathBuf,
    workers: usize,
    preload: bool,
}

#[derive(Clone, Debug)]
enum CliCommand {
    Run {
        config: ChildConfig,
        report: PathBuf,
    },
    Child {
        config: ChildConfig,
    },
}

fn parse_args<I>(args: I) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mode = args
        .next()
        .ok_or_else(|| "缺少 run 或 child 子命令".to_string())?;
    let mut scenario = None;
    let mut file = None;
    let mut workers = None;
    let mut preload = None;
    let mut report = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} 缺少参数值"))?;
        match flag.as_str() {
            "--scenario" => scenario = Some(Scenario::parse(&value)?),
            "--file" => file = Some(PathBuf::from(value)),
            "--workers" => {
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| "workers 必须是 1、2、4 之一".to_string())?;
                if !matches!(parsed, 1 | 2 | 4) {
                    return Err("workers 必须是 1、2、4 之一".into());
                }
                workers = Some(parsed);
            }
            "--preload" => {
                preload = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("preload 必须是 true 或 false".into()),
                });
            }
            "--report" => report = Some(PathBuf::from(value)),
            _ => return Err(format!("未知参数：{flag}")),
        }
    }
    let config = ChildConfig {
        scenario: scenario.ok_or_else(|| "缺少 --scenario".to_string())?,
        file: file.ok_or_else(|| "缺少 --file".to_string())?,
        workers: workers.ok_or_else(|| "缺少 --workers".to_string())?,
        preload: preload.ok_or_else(|| "缺少 --preload".to_string())?,
    };
    match mode.as_str() {
        "run" => Ok(CliCommand::Run {
            config,
            report: report.ok_or_else(|| "run 缺少 --report".to_string())?,
        }),
        "child" => Ok(CliCommand::Child { config }),
        _ => Err(format!("未知子命令：{mode}")),
    }
}

/// 只创建不存在的合成 CSV；既有文件只校验大小，绝不覆盖。
fn ensure_fixture(path: &Path, expected_bytes: u64) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(format!("夹具不是普通文件：{}", path.display()));
            }
            if metadata.len() != expected_bytes {
                return Err(format!(
                    "夹具大小不符：期望 {expected_bytes}，实际 {}",
                    metadata.len(),
                ));
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("读取夹具元数据失败：{error}")),
    }
    if expected_bytes < 7 {
        return Err("合成夹具至少需要 7 字节".into());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建夹具目录失败：{error}"))?;
        }
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建合成夹具失败：{error}"))?;
    let mut writer = std::io::BufWriter::new(file);
    writer
        .write_all(b"value\n")
        .map_err(|error| error.to_string())?;
    let mut remaining = expected_bytes - 6;
    let mut block = [b'0'; 8192];
    for byte in block.iter_mut().skip(7).step_by(8) {
        *byte = b'\n';
    }
    while remaining >= block.len() as u64 {
        writer
            .write_all(&block)
            .map_err(|error| error.to_string())?;
        remaining -= block.len() as u64;
    }
    let tail = remaining as usize;
    if tail == 1 {
        writer.write_all(b"0").map_err(|error| error.to_string())?;
    } else if tail > 1 {
        let zeros = vec![b'0'; tail - 1];
        writer
            .write_all(&zeros)
            .map_err(|error| error.to_string())?;
        writer.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())?;
    Ok(())
}

use simpletable_lib::diagnostics::{
    sample_current_process, sample_process, DiagnosticDocument, LoadEvent, LoadFault, LoadOptions,
    SaveRamEvent, SaveRamPhase,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct SampleSummary {
    private_peak_bytes: u64,
    working_set_peak_bytes: Option<u64>,
    count: u64,
}

#[derive(Default)]
struct SelfSamplerState {
    summary: SampleSummary,
    error: Option<String>,
}

#[derive(Default)]
struct SelfSampler {
    state: Mutex<SelfSamplerState>,
}

impl SelfSampler {
    fn sample(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if state.error.is_some() {
            return;
        }
        match sample_current_process() {
            Ok(sample) => {
                state.summary.private_peak_bytes =
                    state.summary.private_peak_bytes.max(sample.private_bytes);
                if let Some(working_set) = sample.working_set_bytes {
                    state.summary.working_set_peak_bytes = Some(
                        state
                            .summary
                            .working_set_peak_bytes
                            .map_or(working_set, |peak| peak.max(working_set)),
                    );
                }
                state.summary.count += 1;
            }
            Err(error) => state.error = Some(format!("转换器自身采样失败：{error}")),
        }
    }

    fn finish(self) -> Result<SampleSummary, String> {
        let state = self
            .state
            .into_inner()
            .map_err(|_| "转换器自身采样锁已中毒".to_string())?;
        if let Some(error) = state.error {
            return Err(error);
        }
        Ok(state.summary)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ExternalSummary {
    private_peak_bytes: u64,
    working_set_peak_bytes: Option<u64>,
    count: u64,
    max_interval_ms: u64,
}

#[derive(Default)]
struct ExternalMonitorState {
    summary: ExternalSummary,
    error: Option<String>,
}

struct ExternalMonitor {
    stop: std::sync::Arc<AtomicBool>,
    state: std::sync::Arc<Mutex<ExternalMonitorState>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ExternalMonitor {
    /// 启动指定 PID 的真实私有字节监测，采样周期固定为 5 ms。
    fn start(pid: u32) -> Result<Self, String> {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let state = std::sync::Arc::new(Mutex::new(ExternalMonitorState::default()));
        let thread_stop = std::sync::Arc::clone(&stop);
        let thread_state = std::sync::Arc::clone(&state);
        let join = std::thread::Builder::new()
            .name("memory-peak-external-monitor".into())
            .spawn(move || {
                let mut previous = std::time::Instant::now();
                while !thread_stop.load(Ordering::Relaxed) {
                    let sample = sample_process(Some(pid));
                    let now = std::time::Instant::now();
                    let interval = now.duration_since(previous).as_millis() as u64;
                    previous = now;
                    let mut state = match thread_state.lock() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    match sample {
                        Ok(sample) => {
                            state.summary.private_peak_bytes =
                                state.summary.private_peak_bytes.max(sample.private_bytes);
                            if let Some(working_set) = sample.working_set_bytes {
                                state.summary.working_set_peak_bytes = Some(
                                    state
                                        .summary
                                        .working_set_peak_bytes
                                        .map_or(working_set, |peak| peak.max(working_set)),
                                );
                            }
                            state.summary.count += 1;
                            state.summary.max_interval_ms =
                                state.summary.max_interval_ms.max(interval);
                        }
                        Err(error) => {
                            state.error = Some(format!("外部进程采样失败：{error}"));
                            return;
                        }
                    }
                    drop(state);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .map_err(|error| format!("启动外部监测线程失败：{error}"))?;
        Ok(Self {
            stop,
            state,
            join: Some(join),
        })
    }

    /// 停止并 join 监测线程；任何采样错误或线程错误都返回失败。
    fn stop(mut self) -> Result<ExternalSummary, String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| "外部监测线程 panic".to_string())?;
        }
        let state = self
            .state
            .lock()
            .map_err(|_| "外部监测锁已中毒".to_string())?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        Ok(state.summary)
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ReadyMessage {
    baseline_private_bytes: u64,
    reserved_bytes: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ChildResult {
    outcome: OperationOutcome,
    self_samples: SampleSummary,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct OperationOutcome {
    x_bytes: u64,
    iterations: u64,
    commit_order_verified: bool,
    phase_timings: PhaseTimings,
}

/// 各阶段真实耗时（毫秒）；未发生的阶段保持 0。
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseTimings {
    pub convert_ms: u128,
    pub count_ms: u128,
    pub ram_generation_ms: u128,
    pub index_prepare_ms: u128,
    pub index_commit_ms: u128,
    pub disk_write_ms: u128,
    pub total_ms: u128,
}

/// 保存阶段中尚未闭合的计时起点；重复出现时由 `+=` 累加。
#[derive(Default)]
struct PhaseClock {
    count_started: Option<Instant>,
    new_reserve_started: Option<Instant>,
    history_started: Option<Instant>,
    ready_started: Option<Instant>,
    disk_started: Option<Instant>,
}

/// 执行一次真实转换（可取消/注入故障），并把转换耗时写入共享计时。
///
/// 参数：
/// - `timings`：跨多次转换累加的阶段计时；`convert_ms` 取 `ReserveBegin→Finished`，
///   取消或失败没有 `Finished`，在调用返回处闭合；`total_ms` 累加整个调用耗时。
///
/// 返回：是否真正进入内存模式；转换本身失败时返回原有错误。
fn enter_with_optional_cancel(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    cancel_after_quarter: bool,
    fault: simpletable_lib::diagnostics::LoadFault,
    sampler: &SelfSampler,
    timings: &Mutex<PhaseTimings>,
) -> Result<bool, String> {
    let call_started = Instant::now();
    let convert_started = Mutex::new(None::<Instant>);
    let cancel = AtomicBool::new(false);
    let options = LoadOptions {
        workers: config.workers,
        fault,
        ..LoadOptions::default()
    };
    let result = document.enter_memory_mode(&cancel, options, |event| {
        sampler.sample();
        match event {
            LoadEvent::ReserveBegin { .. } => {
                *convert_started.lock().unwrap() = Some(Instant::now());
            }
            LoadEvent::Finished { .. } => {
                let started = convert_started.lock().unwrap().take();
                if let Some(started) = started {
                    timings.lock().unwrap().convert_ms += started.elapsed().as_millis();
                }
            }
            _ => {}
        }
        if cancel_after_quarter {
            if let LoadEvent::Progress { done, total } = event {
                if total > 0 && done > total / 4 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
    });
    // 取消或失败没有 Finished：在转换调用返回处闭合未闭合区间。
    let open = convert_started.into_inner().unwrap_or(None);
    if let Some(started) = open {
        timings.lock().unwrap().convert_ms += started.elapsed().as_millis();
    }
    timings.lock().unwrap().total_ms += call_started.elapsed().as_millis();
    result
}

fn expected_fault_recovers(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    sampler: &SelfSampler,
    fault: simpletable_lib::diagnostics::LoadFault,
    expected_error: &str,
    timings: &Mutex<PhaseTimings>,
) -> Result<(), String> {
    let reserved_before = DiagnosticDocument::live_reserved_bytes();
    let error = match enter_with_optional_cancel(document, config, false, fault, sampler, timings) {
        Ok(_) => return Err("故障场景意外完成转换".into()),
        Err(error) => error,
    };
    if !error.contains(expected_error) {
        return Err(format!(
            "故障错误不符：期望包含 {expected_error}，实际 {error}"
        ));
    }
    let reserved_after = DiagnosticDocument::live_reserved_bytes();
    if reserved_after != reserved_before {
        return Err(format!(
            "故障恢复遗留区域：之前 {reserved_before}，之后 {reserved_after}"
        ));
    }
    Ok(())
}

/// 转换类场景在各分支检查全部通过后构造结果。
///
/// `commit_order_verified` 表示该分支自己的真实断言（进入/取消/故障恢复/区域无遗留）
/// 已全部通过，而不是把未检查的 `false` 机械改成 `true`。
fn checked_conversion_outcome(
    x_bytes: u64,
    iterations: u64,
    phase_timings: PhaseTimings,
) -> OperationOutcome {
    OperationOutcome {
        x_bytes,
        iterations,
        commit_order_verified: true,
        phase_timings,
    }
}

/// 验证一次持久内存操作的阶段顺序。
///
/// 参数：
/// - `phases`：真实 observer 收集到的 `SaveRamPhase` 序列。
/// - `expected_materializations`：允许出现的 `NewRamReady` 次数；正文未变化的重试必须仍为 1。
/// - `require_disk_save`：是否要求磁盘写入与提交事件；只整理 RAM 的列操作必须为 `false`。
///
/// 返回：物化次数精确、`NewRamReady` 先于 `OldRamReleased`，且磁盘事件符合要求时为 `true`。
fn commit_order_verified(
    phases: &[SaveRamPhase],
    expected_materializations: usize,
    require_disk_save: bool,
) -> bool {
    let position = |predicate: &dyn Fn(&SaveRamPhase) -> bool| phases.iter().position(predicate);
    let ready = position(&|phase| matches!(phase, SaveRamPhase::NewRamReady { .. }));
    let released = position(&|phase| matches!(phase, SaveRamPhase::OldRamReleased { .. }));
    let disk = position(&|phase| matches!(phase, SaveRamPhase::DiskWriteBegin { .. }));
    let committed = position(&|phase| matches!(phase, SaveRamPhase::DiskCommitted { .. }));
    let materializations = phases
        .iter()
        .filter(|phase| matches!(phase, SaveRamPhase::NewRamReady { .. }))
        .count();
    if materializations != expected_materializations
        || !matches!((ready, released), (Some(a), Some(b)) if a < b)
    {
        return false;
    }
    if require_disk_save {
        matches!((ready, released, disk, committed),
            (Some(a), Some(b), Some(c), Some(d)) if a < b && b < c && c < d)
    } else {
        disk.is_none() && committed.is_none()
    }
}

/// 执行四个真实持久内存场景，并验证 RAM/磁盘提交顺序与内容。
///
/// 参数：
/// - `document`：已进入内存模式的真实文档。
/// - `config`：场景、源文件与并发配置。
/// - `sampler`：阶段事件到达时的自身内存采样器。
///
/// 返回：包含验证结果与阶段计时的操作结果。
fn execute_persistent_operation(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    sampler: &SelfSampler,
) -> Result<OperationOutcome, String> {
    let destination = config.file.with_extension("save.csv");
    let source_metadata = std::fs::metadata(&config.file)
        .map_err(|error| format!("读取操作前源文件失败：{error}"))?;
    let source_bytes = source_metadata.len();
    let source_modified = source_metadata
        .modified()
        .map_err(|error| format!("读取操作前源文件 mtime 失败：{error}"))?;
    let original_first = document
        .rows(0, 1)
        .into_iter()
        .next()
        .and_then(|row| row.first().cloned())
        .ok_or("持久内存峰值夹具缺少首行首列")?;
    let load_options = LoadOptions {
        workers: config.workers,
        fault: LoadFault::None,
        ..LoadOptions::default()
    };
    let phases = Mutex::new(Vec::with_capacity(32));
    let phase_timings = Mutex::new(PhaseTimings::default());
    let clock = Mutex::new(PhaseClock::default());
    let observe = |event| {
        sampler.sample();
        let SaveRamEvent::Phase(phase) = event;
        let now = Instant::now();
        phases.lock().unwrap().push(phase);
        // 每个区间单独闭合；同一相位重复出现时累加，不把总耗时填进每个字段。
        let mut clock = clock.lock().unwrap();
        let mut timings = phase_timings.lock().unwrap();
        match phase {
            SaveRamPhase::CountBegin => clock.count_started = Some(now),
            SaveRamPhase::CountFinished { .. } => {
                if let Some(started) = clock.count_started.take() {
                    timings.count_ms += now.duration_since(started).as_millis();
                }
            }
            SaveRamPhase::HistoryPrepared => clock.history_started = Some(now),
            SaveRamPhase::IndexPrepared { .. } => {
                if let Some(started) = clock.history_started.take() {
                    timings.index_prepare_ms += now.duration_since(started).as_millis();
                }
            }
            SaveRamPhase::NewReserveBegin { .. } => clock.new_reserve_started = Some(now),
            SaveRamPhase::NewRamReady { .. } => {
                if let Some(started) = clock.new_reserve_started.take() {
                    timings.ram_generation_ms += now.duration_since(started).as_millis();
                }
                clock.ready_started = Some(now);
            }
            SaveRamPhase::OldRamReleased { .. } => {
                if let Some(started) = clock.ready_started.take() {
                    timings.index_commit_ms += now.duration_since(started).as_millis();
                }
            }
            SaveRamPhase::DiskWriteBegin { .. } => clock.disk_started = Some(now),
            SaveRamPhase::DiskWriteFinished { .. } => {
                if let Some(started) = clock.disk_started.take() {
                    timings.disk_write_ms += now.duration_since(started).as_millis();
                }
            }
            _ => {}
        }
    };
    let operation_started = Instant::now();
    match config.scenario {
        Scenario::Save => {
            document.set_cell(0, 0, "edited".into())?;
            document.save_with_ram_observer(&destination, load_options, &observe)?;
        }
        Scenario::ColumnApply => {
            document.insert_column_with_ram_observer(1, "note".into(), &observe)?;
        }
        Scenario::ColumnSave => {
            document.insert_column_with_ram_observer(1, "note".into(), &observe)?;
            document.save_with_ram_observer(&destination, load_options, &observe)?;
        }
        Scenario::SaveRetry => {
            document.set_cell(0, 0, "edited".into())?;
            document.save_with_ram_observer(&destination, load_options, &observe)?;
            let first_bytes = std::fs::metadata(&destination)
                .map_err(|error| format!("读取第一次保存失败：{error}"))?
                .len();
            document.save_with_ram_observer(&destination, load_options, &observe)?;
            let second_bytes = std::fs::metadata(&destination)
                .map_err(|error| format!("读取第二次保存失败：{error}"))?
                .len();
            if first_bytes != second_bytes {
                return Err(format!(
                    "同 RAM 重试文件长度变化：{first_bytes} != {second_bytes}"
                ));
            }
        }
        _ => unreachable!("只有四个持久内存场景进入此函数"),
    }
    // 结束 observer 对阶段容器的借用后才能取回真实事件序列。
    let _ = observe;
    let phases = phases
        .into_inner()
        .map_err(|_| "保存事件锁已中毒".to_string())?;
    let mut phase_timings = phase_timings
        .into_inner()
        .map_err(|_| "阶段计时锁已中毒".to_string())?;
    phase_timings.total_ms = operation_started.elapsed().as_millis();
    let verified = commit_order_verified(&phases, 1, requires_disk_save(config.scenario));
    if !verified {
        return Err(format!("RAM/磁盘提交顺序不符：{phases:?}"));
    }
    let new_ram_bytes = phases
        .iter()
        .find_map(|phase| match phase {
            SaveRamPhase::NewRamReady { new_bytes } => Some(*new_bytes),
            _ => None,
        })
        .ok_or("缺少 NewRamReady 字节数")?;
    let output_bytes = if requires_disk_save(config.scenario) {
        std::fs::metadata(&destination)
            .map_err(|error| format!("读取磁盘提交后的目标失败：{error}"))?
            .len()
    } else {
        let after = std::fs::metadata(&config.file)
            .map_err(|error| format!("读取列物化后的源文件失败：{error}"))?;
        let after_modified = after
            .modified()
            .map_err(|error| format!("读取列物化后的源文件 mtime 失败：{error}"))?;
        if after.len() != source_bytes || after_modified != source_modified {
            return Err("执行列操作改变了源文件大小或 mtime".into());
        }
        new_ram_bytes
    };
    if config.scenario == Scenario::ColumnSave && output_bytes <= source_bytes {
        return Err("真实插列保存后文件未增大".into());
    }
    let rows = document.rows(0, 1);
    let first = rows.first().ok_or("操作后首行消失")?;
    match config.scenario {
        Scenario::Save | Scenario::SaveRetry => {
            if first.first().map(String::as_str) != Some("edited") {
                return Err(format!("单元格保存内容错误：{rows:?}"));
            }
        }
        Scenario::ColumnApply | Scenario::ColumnSave => {
            if first.get(..2) != Some(&[original_first, String::new()][..]) {
                return Err(format!("插列后的内容错误：{rows:?}"));
            }
        }
        _ => unreachable!(),
    }
    Ok(OperationOutcome {
        x_bytes: source_bytes.max(new_ram_bytes).max(output_bytes),
        iterations: 1,
        commit_order_verified: verified,
        phase_timings,
    })
}

fn execute_operation(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    sampler: &SelfSampler,
) -> Result<OperationOutcome, String> {
    let x_bytes = std::fs::metadata(&config.file)
        .map_err(|error| format!("读取场景文件大小失败：{error}"))?
        .len();
    match config.scenario {
        Scenario::Convert => {
            let timings = Mutex::new(PhaseTimings::default());
            if !enter_with_optional_cancel(
                document,
                config,
                false,
                simpletable_lib::diagnostics::LoadFault::None,
                sampler,
                &timings,
            )? {
                return Err("普通转换未进入内存模式".into());
            }
            let phase_timings = timings
                .into_inner()
                .map_err(|_| "阶段计时锁已中毒".to_string())?;
            Ok(checked_conversion_outcome(x_bytes, 1, phase_timings))
        }
        Scenario::Cancel => {
            let reserved_before = DiagnosticDocument::live_reserved_bytes();
            let timings = Mutex::new(PhaseTimings::default());
            if enter_with_optional_cancel(
                document,
                config,
                true,
                simpletable_lib::diagnostics::LoadFault::None,
                sampler,
                &timings,
            )? {
                return Err("取消场景意外进入内存模式".into());
            }
            let reserved_after = DiagnosticDocument::live_reserved_bytes();
            if reserved_after != reserved_before {
                return Err(format!(
                    "取消恢复遗留区域：之前 {reserved_before}，之后 {reserved_after}"
                ));
            }
            let phase_timings = timings
                .into_inner()
                .map_err(|_| "阶段计时锁已中毒".to_string())?;
            Ok(checked_conversion_outcome(x_bytes, 1, phase_timings))
        }
        Scenario::CancelLoop => {
            let reserved_before = DiagnosticDocument::live_reserved_bytes();
            // 三次取消共用同一份计时：convert_ms 与 total_ms 都累加。
            let timings = Mutex::new(PhaseTimings::default());
            for iteration in 0..3 {
                if enter_with_optional_cancel(
                    document,
                    config,
                    true,
                    simpletable_lib::diagnostics::LoadFault::None,
                    sampler,
                    &timings,
                )? {
                    return Err(format!("第 {} 次取消意外进入内存模式", iteration + 1));
                }
                let reserved_after = DiagnosticDocument::live_reserved_bytes();
                if reserved_after != reserved_before {
                    return Err(format!(
                        "第 {} 次取消遗留区域：之前 {reserved_before}，之后 {reserved_after}",
                        iteration + 1,
                    ));
                }
            }
            let phase_timings = timings
                .into_inner()
                .map_err(|_| "阶段计时锁已中毒".to_string())?;
            Ok(checked_conversion_outcome(x_bytes, 3, phase_timings))
        }
        Scenario::CommitFail => {
            let timings = Mutex::new(PhaseTimings::default());
            expected_fault_recovers(
                document,
                config,
                sampler,
                simpletable_lib::diagnostics::LoadFault::CommitAt(0),
                "注入提交失败",
                &timings,
            )?;
            let phase_timings = timings
                .into_inner()
                .map_err(|_| "阶段计时锁已中毒".to_string())?;
            Ok(checked_conversion_outcome(x_bytes, 1, phase_timings))
        }
        Scenario::ReadFail => {
            let timings = Mutex::new(PhaseTimings::default());
            expected_fault_recovers(
                document,
                config,
                sampler,
                simpletable_lib::diagnostics::LoadFault::ReadAfter(0),
                "注入读取失败",
                &timings,
            )?;
            let phase_timings = timings
                .into_inner()
                .map_err(|_| "阶段计时锁已中毒".to_string())?;
            Ok(checked_conversion_outcome(x_bytes, 1, phase_timings))
        }
        Scenario::Save | Scenario::ColumnApply | Scenario::ColumnSave | Scenario::SaveRetry => {
            execute_persistent_operation(document, config, sampler)
        }
    }
}

fn write_protocol_message<T>(output: &mut dyn Write, prefix: &str, value: &T) -> Result<(), String>
where
    T: serde::Serialize,
{
    let json =
        serde_json::to_string(value).map_err(|error| format!("序列化{prefix}消息失败：{error}"))?;
    writeln!(output, "{prefix} {json}")
        .map_err(|error| format!("写出{prefix}消息失败：{error}"))?;
    output
        .flush()
        .map_err(|error| format!("flush {prefix} 消息失败：{error}"))
}

fn expect_protocol_command(input: &mut dyn std::io::BufRead, expected: &str) -> Result<(), String> {
    let mut line = String::new();
    let count = input
        .read_line(&mut line)
        .map_err(|error| format!("等待 {expected} 失败：{error}"))?;
    if count == 0 {
        return Err(format!("等待 {expected} 时输入已关闭"));
    }
    if line.trim() != expected {
        return Err(format!(
            "协议命令错误：期望 {expected}，实际 {}",
            line.trim()
        ));
    }
    Ok(())
}

/// 准备真实文档，READY 后等待 GO，执行场景并返回完整自身采样结果。
fn run_child_with_io(
    config: &ChildConfig,
    input: &mut dyn std::io::BufRead,
    output: &mut dyn Write,
) -> Result<ChildResult, String> {
    let mut document = DiagnosticDocument::open_complete(&config.file)?;
    if config.preload {
        document.preload_source();
    }

    // 在基线前预热事件和协议报告分配，避免把运行时准备算入转换峰值。
    let mut progress_prewarm = Vec::with_capacity(1024);
    progress_prewarm.push(0u64);
    let mut report_prewarm = Vec::with_capacity(16 * 1024);
    report_prewarm.extend_from_slice(b"{\"prewarm\":true}");
    std::hint::black_box(&progress_prewarm);
    std::hint::black_box(&report_prewarm);
    drop(progress_prewarm);
    drop(report_prewarm);

    if requires_memory_mode(config.scenario) {
        let warmup_sampler = SelfSampler::default();
        // READY 前的预进入不计入保存场景的阶段性计时。
        let warmup_timings = Mutex::new(PhaseTimings::default());
        if !enter_with_optional_cancel(
            &mut document,
            config,
            false,
            simpletable_lib::diagnostics::LoadFault::None,
            &warmup_sampler,
            &warmup_timings,
        )? {
            return Err("持久内存场景未能预先进入内存模式".into());
        }
        std::hint::black_box(warmup_sampler.finish()?);
    }

    let sampler = SelfSampler::default();
    sampler.sample();
    let baseline_private_bytes = sample_current_process()
        .map_err(|error| format!("记录转换前基线失败：{error}"))?
        .private_bytes;
    let ready = ReadyMessage {
        baseline_private_bytes,
        reserved_bytes: DiagnosticDocument::live_reserved_bytes(),
    };
    write_protocol_message(output, "READY", &ready)?;
    expect_protocol_command(input, "GO")?;

    let outcome = execute_operation(&mut document, config, &sampler)?;
    let self_samples = sampler.finish()?;
    let result = ChildResult {
        outcome,
        self_samples,
    };
    write_protocol_message(output, "RESULT", &result)?;
    // 保持进程存活到父进程停止外部监测，避免退出被误记为采样错误。
    expect_protocol_command(input, "DONE")?;
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct PeakInput {
    pub profile: String,
    pub scenario: String,
    pub x_bytes: u64,
    pub baseline_private_bytes: u64,
    pub self_peak_private_bytes: u64,
    pub external_peak_private_bytes: u64,
    pub working_set_peak_bytes: Option<u64>,
    pub self_sample_count: u64,
    pub external_sample_count: u64,
    pub external_max_interval_ms: u64,
    pub commit_order_verified: bool,
    pub phase_timings: PhaseTimings,
}

#[cfg(test)]
impl PeakInput {
    fn test_defaults() -> Self {
        Self {
            profile: "test".into(),
            scenario: "convert".into(),
            x_bytes: 64 * 1024 * 1024,
            baseline_private_bytes: 100,
            self_peak_private_bytes: 100,
            external_peak_private_bytes: 100,
            working_set_peak_bytes: Some(200),
            self_sample_count: 2,
            external_sample_count: 2,
            external_max_interval_ms: 5,
            commit_order_verified: true,
            phase_timings: PhaseTimings::default(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeakReport {
    pub platform: String,
    pub profile: String,
    pub scenario: String,
    pub file_bytes_x: u64,
    pub allowed_peak_increment_bytes: u64,
    pub baseline_private_bytes: u64,
    pub self_peak_private_bytes: u64,
    pub external_peak_private_bytes: u64,
    pub observed_peak_private_bytes: u64,
    pub actual_peak_increment_bytes: i128,
    pub remaining_bytes: i128,
    pub excess_bytes: i128,
    pub working_set_peak_bytes: Option<u64>,
    pub self_sample_count: u64,
    pub external_sample_count: u64,
    pub monitor_interval_ms: u32,
    pub monitor_max_interval_ms: u64,
    pub commit_order_verified: bool,
    pub phase_timings: PhaseTimings,
    pub system_cache_note: String,
    pub pass: bool,
}

/// 固定的内存口径说明：系统缓存不计入进程私有匿名 RAM。
const SYSTEM_CACHE_NOTE: &str =
    "系统文件缓存和备用列表不计入进程私有匿名 RAM；本报告未将其作为应用泄漏";

pub fn evaluate_peak(input: PeakInput) -> Result<PeakReport, String> {
    if input.self_sample_count == 0 {
        return Err("缺少转换器自身采样".into());
    }
    if input.external_sample_count == 0 {
        return Err("缺少外部采样".into());
    }
    let allowed_u128 = (input.x_bytes as u128)
        .checked_mul(1050)
        .and_then(|value| value.checked_div(1000))
        .ok_or_else(|| "允许峰值计算溢出".to_string())?;
    let allowed_u64 =
        u64::try_from(allowed_u128).map_err(|_| "允许峰值超出 u64 报告范围".to_string())?;
    let allowed = allowed_u128 as i128;
    let baseline = input.baseline_private_bytes as i128;
    let observed = input
        .self_peak_private_bytes
        .max(input.external_peak_private_bytes) as i128;
    let actual = observed - baseline;
    let remaining = allowed - actual;
    Ok(PeakReport {
        platform: std::env::consts::OS.into(),
        profile: input.profile,
        scenario: input.scenario,
        file_bytes_x: input.x_bytes,
        allowed_peak_increment_bytes: allowed_u64,
        baseline_private_bytes: input.baseline_private_bytes,
        self_peak_private_bytes: input.self_peak_private_bytes,
        external_peak_private_bytes: input.external_peak_private_bytes,
        observed_peak_private_bytes: observed as u64,
        actual_peak_increment_bytes: actual,
        remaining_bytes: remaining.max(0),
        excess_bytes: (-remaining).max(0),
        working_set_peak_bytes: input.working_set_peak_bytes,
        self_sample_count: input.self_sample_count,
        external_sample_count: input.external_sample_count,
        monitor_interval_ms: 5,
        monitor_max_interval_ms: input.external_max_interval_ms,
        commit_order_verified: input.commit_order_verified,
        phase_timings: input.phase_timings,
        system_cache_note: SYSTEM_CACHE_NOTE.into(),
        // 字节达标但提交顺序未验证时整体判失败。
        pass: actual <= allowed && input.commit_order_verified,
    })
}

/// 写出完整 JSON 报告及单行 PASS/FAIL 摘要，返回 I/O 或序列化错误。
fn write_report(report: &PeakReport, path: &Path, output: &mut dyn Write) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建报告目录失败：{error}"))?;
        }
    }
    let mut file = std::fs::File::create(path)
        .map_err(|error| format!("创建峰值报告失败 {}：{error}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, report)
        .map_err(|error| format!("序列化峰值报告失败：{error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("写入峰值报告换行失败：{error}"))?;
    file.flush()
        .map_err(|error| format!("flush 峰值报告失败：{error}"))?;
    let json =
        serde_json::to_string(report).map_err(|error| format!("序列化控制台摘要失败：{error}"))?;
    let outcome = if report.pass { "PASS" } else { "FAIL" };
    writeln!(output, "PEAK {outcome} {json}")
        .map_err(|error| format!("写出控制台摘要失败：{error}"))
}

fn report_exit_code(report: &PeakReport) -> i32 {
    if report.pass {
        0
    } else {
        1
    }
}

fn fixture_size_from_name(path: &Path) -> Result<u64, String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("夹具文件名不是 UTF-8：{}", path.display()))?;
    let mib = stem
        .strip_prefix("fixture-")
        .ok_or_else(|| format!("夹具名必须为 fixture-N.csv：{}", path.display()))?
        .parse::<u64>()
        .map_err(|_| format!("夹具名必须包含 MiB 整数：{}", path.display()))?;
    mib.checked_mul(1024 * 1024)
        .ok_or_else(|| "夹具 MiB 大小溢出".to_string())
}

/// 通过真实 stdin/stdout 执行子进程，错误同时以协议行通知父进程。
fn run_child_stdio(config: &ChildConfig) -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = std::io::BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    match run_child_with_io(config, &mut input, &mut output) {
        Ok(_) => Ok(()),
        Err(error) => {
            let _ = write_protocol_message(&mut output, "ERROR", &error);
            Err(error)
        }
    }
}

fn read_protocol_message<T>(reader: &mut dyn std::io::BufRead, prefix: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let expected = format!("{prefix} ");
    loop {
        let mut line = String::new();
        let count = reader
            .read_line(&mut line)
            .map_err(|error| format!("读取子进程 {prefix} 失败：{error}"))?;
        if count == 0 {
            return Err(format!("子进程在 {prefix} 前关闭输出"));
        }
        if let Some(json) = line.strip_prefix("ERROR ") {
            let error =
                serde_json::from_str::<String>(json).unwrap_or_else(|_| json.trim().to_string());
            return Err(format!("子进程失败：{error}"));
        }
        if let Some(json) = line.strip_prefix(expected.as_str()) {
            return serde_json::from_str(json)
                .map_err(|error| format!("解析子进程 {prefix} 失败：{error}"));
        }
        // 集成测试的 libtest 会输出自己的测试行；真实 child 只输出协议行。
    }
}

struct ChildProcess(std::process::Child);

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 运行一个真实子进程并用外部 5 ms 监测求值 105% 硬门槛。
fn run_parent_with_command(
    config: &ChildConfig,
    report_path: &Path,
    command: &mut std::process::Command,
    output: &mut dyn Write,
) -> Result<PeakReport, String> {
    ensure_fixture(&config.file, fixture_size_from_name(&config.file)?)?;
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let mut child = ChildProcess(
        command
            .spawn()
            .map_err(|error| format!("启动内存峰值的子进程失败：{error}"))?,
    );
    let pid = child.0.id();
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| "缺少子进程 stdin 管道".to_string())?;
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| "缺少子进程 stdout 管道".to_string())?;
    let mut reader = std::io::BufReader::new(stdout);
    let ready: ReadyMessage = read_protocol_message(&mut reader, "READY")?;

    // 在 GO 前验证外部采样权限，避免开始转换后才因权限失败。
    sample_process(Some(pid)).map_err(|error| format!("外部采样预检失败：{error}"))?;
    let monitor = ExternalMonitor::start(pid)?;
    let child_result = (|| -> Result<ChildResult, String> {
        writeln!(input, "GO").map_err(|error| format!("发送 GO 失败：{error}"))?;
        input
            .flush()
            .map_err(|error| format!("flush GO 失败：{error}"))?;
        read_protocol_message(&mut reader, "RESULT")
    })();
    let external_result = monitor.stop();
    let result = child_result?;
    let external = external_result?;
    writeln!(input, "DONE").map_err(|error| format!("发送 DONE 失败：{error}"))?;
    input
        .flush()
        .map_err(|error| format!("flush DONE 失败：{error}"))?;
    let status = child
        .0
        .wait()
        .map_err(|error| format!("等待子进程退出失败：{error}"))?;
    if !status.success() {
        return Err(format!("子进程退出状态失败：{status}"));
    }

    let profile = format!(
        "{}-workers-{}-preload-{}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        config.workers,
        config.preload,
    );
    let report = evaluate_peak(PeakInput {
        profile,
        scenario: config.scenario.as_str().into(),
        x_bytes: result.outcome.x_bytes,
        baseline_private_bytes: ready.baseline_private_bytes,
        self_peak_private_bytes: result.self_samples.private_peak_bytes,
        external_peak_private_bytes: external.private_peak_bytes,
        working_set_peak_bytes: result
            .self_samples
            .working_set_peak_bytes
            .max(external.working_set_peak_bytes),
        self_sample_count: result.self_samples.count,
        external_sample_count: external.count,
        external_max_interval_ms: external.max_interval_ms,
        commit_order_verified: result.outcome.commit_order_verified,
        phase_timings: result.outcome.phase_timings,
    })?;
    write_report(&report, report_path, output)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    pub(super) static REAL_MEMORY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    use super::{evaluate_peak, PeakInput};
    use simpletable_lib::diagnostics::SaveRamPhase;

    /// 构造一次“物化 → 释放旧 RAM → 写盘 → 提交”的合法阶段序列。
    fn ordered_save_phases() -> Vec<SaveRamPhase> {
        vec![
            SaveRamPhase::NewRamReady { new_bytes: 8 },
            SaveRamPhase::OldRamReleased { old_bytes: 8 },
            SaveRamPhase::DiskWriteBegin { new_bytes: 8 },
            SaveRamPhase::DiskCommitted { new_bytes: 8 },
        ]
    }

    #[test]
    fn commit_order_requires_ram_before_disk_and_exact_materialization_count() {
        let valid = ordered_save_phases();
        assert!(super::commit_order_verified(&valid, 1, true));
        assert!(
            !super::commit_order_verified(&valid, 2, true),
            "重试不得再次物化"
        );

        let mut early_release = ordered_save_phases();
        early_release.swap(0, 1);
        assert!(!super::commit_order_verified(&early_release, 1, true));
        assert!(
            !super::commit_order_verified(&valid[..3], 1, true),
            "缺 DiskCommitted"
        );
    }

    #[test]
    fn column_apply_commit_order_forbids_all_disk_events() {
        let ram_only = ordered_save_phases()[..2].to_vec();
        assert!(super::commit_order_verified(&ram_only, 1, false));
        assert!(!super::commit_order_verified(
            &ordered_save_phases(),
            1,
            false
        ));
    }

    #[test]
    fn peak_gate_uses_floor_105_percent_and_separates_absolute_values() {
        let mut input = PeakInput::test_defaults();
        input.x_bytes = 101;
        input.baseline_private_bytes = 1_000;
        input.self_peak_private_bytes = 1_106;
        input.external_peak_private_bytes = 1_105;
        input.working_set_peak_bytes = Some(9_999);
        let report = evaluate_peak(input).unwrap();
        // floor(101 × 1.05) = 106，不能向上取整成 107。
        assert_eq!(report.allowed_peak_increment_bytes, 106);
        assert_eq!(report.observed_peak_private_bytes, 1_106);
        assert_eq!(report.actual_peak_increment_bytes, 106);
        assert_eq!(report.working_set_peak_bytes, Some(9_999));
        assert!(report.pass);

        let mut over = PeakInput::test_defaults();
        over.x_bytes = 101;
        over.baseline_private_bytes = 1_000;
        over.self_peak_private_bytes = 1_107;
        over.external_peak_private_bytes = 1_000;
        let report = evaluate_peak(over).unwrap();
        assert_eq!(report.excess_bytes, 1);
        assert!(!report.pass);
    }

    #[test]
    fn peak_gate_rejects_bytes_that_pass_without_verified_commit_order() {
        let mut input = PeakInput::test_defaults();
        input.commit_order_verified = false;
        let report = evaluate_peak(input).unwrap();
        assert!(report.actual_peak_increment_bytes <= report.allowed_peak_increment_bytes as i128);
        assert!(!report.commit_order_verified);
        assert!(!report.pass);
    }

    #[test]
    fn peak_hard_limit_rejects_one_byte_excess() {
        let x = 64 * 1024 * 1024;
        let allowed = x * 105 / 100;
        let base = PeakInput {
            x_bytes: x,
            baseline_private_bytes: 100,
            self_peak_private_bytes: 100 + allowed,
            external_peak_private_bytes: 100,
            self_sample_count: 2,
            external_sample_count: 2,
            ..PeakInput::test_defaults()
        };
        let pass = evaluate_peak(base.clone()).unwrap();
        assert!(pass.pass);
        assert_eq!(pass.remaining_bytes, 0);

        let fail = evaluate_peak(PeakInput {
            external_peak_private_bytes: 101 + allowed,
            ..base
        })
        .unwrap();
        assert!(!fail.pass);
        assert_eq!(fail.excess_bytes, 1);
    }

    #[test]
    fn missing_external_samples_is_failure_not_a_pass_by_default() {
        let error = evaluate_peak(PeakInput {
            external_sample_count: 0,
            ..PeakInput::test_defaults()
        })
        .unwrap_err();
        assert!(error.contains("外部采样"));
    }

    #[test]
    fn missing_self_samples_is_failure_not_a_pass_by_default() {
        let error = evaluate_peak(PeakInput {
            self_sample_count: 0,
            ..PeakInput::test_defaults()
        })
        .unwrap_err();
        assert!(error.contains("自身采样"));
    }

    #[test]
    fn diagnostics_use_real_document_and_process_sampler() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use simpletable_lib::diagnostics::{
            sample_current_process, DiagnosticDocument, LoadOptions,
        };
        use std::sync::atomic::AtomicBool;
        use std::sync::Mutex;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let source = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-source-{}-{unique}.csv",
            std::process::id(),
        ));
        let destination = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-destination-{}-{unique}.csv",
            std::process::id(),
        ));
        std::fs::write(&source, b"a,b\n1,2\n3,4\n").unwrap();
        let mut document = DiagnosticDocument::open_complete(&source).unwrap();
        assert_eq!(
            document.backing_file_bytes(),
            std::fs::metadata(&source).unwrap().len(),
        );
        assert_eq!(document.rows(0, 2).len(), 2);
        document.preload_source();
        let error = document
            .enter_memory_mode(&AtomicBool::new(false), LoadOptions::default(), |_| {})
            .unwrap_err();
        assert!(error.contains("50 MiB"));
        let original_first = document.rows(0, 1)[0][0].clone();
        document.insert_column(1, "note".into()).unwrap();
        let events = Mutex::new(Vec::new());
        document
            .save_with_ram_observer(&destination, LoadOptions::default(), |event| {
                events.lock().unwrap().push(event);
            })
            .unwrap();
        assert!(destination.exists());
        assert_eq!(document.rows(0, 1)[0][..2], [original_first, String::new()]);
        assert!(sample_current_process().unwrap().private_bytes > 0);
        assert_eq!(
            DiagnosticDocument::live_reserved_bytes(),
            DiagnosticDocument::live_reserved_bytes(),
        );
    }

    #[test]
    fn cli_parses_run_and_child_with_bounded_workers() {
        use super::{parse_args, CliCommand};

        let run = parse_args([
            "run".into(),
            "--scenario".into(),
            "convert".into(),
            "--file".into(),
            "fixture-64.csv".into(),
            "--workers".into(),
            "4".into(),
            "--preload".into(),
            "true".into(),
            "--report".into(),
            "report.json".into(),
        ])
        .unwrap();
        let CliCommand::Run { config, report } = run else {
            panic!("应解析为 run");
        };
        assert_eq!(config.scenario.as_str(), "convert");
        assert_eq!(config.workers, 4);
        assert!(config.preload);
        assert_eq!(report, std::path::PathBuf::from("report.json"));

        let child = parse_args([
            "child".into(),
            "--scenario".into(),
            "cancel-loop".into(),
            "--file".into(),
            "fixture-64.csv".into(),
            "--workers".into(),
            "2".into(),
            "--preload".into(),
            "false".into(),
        ])
        .unwrap();
        assert!(matches!(child, CliCommand::Child { .. }));
        let error = parse_args([
            "run".into(),
            "--scenario".into(),
            "convert".into(),
            "--file".into(),
            "fixture-64.csv".into(),
            "--workers".into(),
            "3".into(),
            "--preload".into(),
            "false".into(),
            "--report".into(),
            "report.json".into(),
        ])
        .unwrap_err();
        assert!(error.contains("1、2、4"));
    }

    #[test]
    fn fixture_generation_is_exact_and_never_overwrites_existing_data() {
        use super::ensure_fixture;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-fixture-{}-{unique}.csv",
            std::process::id(),
        ));
        ensure_fixture(&path, 64 * 1024).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024);
        assert_eq!(&std::fs::read(&path).unwrap()[..6], b"value\n");
        let error = ensure_fixture(&path, 32 * 1024).unwrap_err();
        assert!(error.contains("大小"));
    }

    #[test]
    fn convert_operation_uses_real_engine_and_allocation_observers() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use super::{ensure_fixture, execute_operation, ChildConfig, Scenario, SelfSampler};
        use simpletable_lib::diagnostics::DiagnosticDocument;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-convert-{}-{unique}.csv",
            std::process::id(),
        ));
        let file_bytes = 50 * 1024 * 1024;
        ensure_fixture(&path, file_bytes).unwrap();
        let mut document = DiagnosticDocument::open_complete(&path).unwrap();
        let config = ChildConfig {
            scenario: Scenario::Convert,
            file: path,
            workers: 2,
            preload: false,
        };
        let sampler = SelfSampler::default();
        let outcome = execute_operation(&mut document, &config, &sampler).unwrap();
        assert_eq!(outcome.x_bytes, file_bytes);
        assert_eq!(outcome.iterations, 1);
        // 转换计时来自 ReserveBegin→Finished，必须真实非零。
        assert!(outcome.phase_timings.convert_ms > 0);
        assert!(outcome.phase_timings.total_ms >= outcome.phase_timings.convert_ms);
        let samples = sampler.finish().unwrap();
        assert!(samples.count > 4);
        assert!(samples.private_peak_bytes > 0);
        assert_eq!(document.rows(0, 1).len(), 1);
    }

    #[test]
    fn cancel_loop_restores_reserved_bytes_three_times() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use super::{ensure_fixture, execute_operation, ChildConfig, Scenario, SelfSampler};
        use simpletable_lib::diagnostics::DiagnosticDocument;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-cancel-loop-{}-{unique}.csv",
            std::process::id(),
        ));
        ensure_fixture(&path, 50 * 1024 * 1024).unwrap();
        let mut document = DiagnosticDocument::open_complete(&path).unwrap();
        let reserved_before = DiagnosticDocument::live_reserved_bytes();
        let config = ChildConfig {
            scenario: Scenario::CancelLoop,
            file: path,
            workers: 2,
            preload: false,
        };
        let sampler = SelfSampler::default();
        let outcome = execute_operation(&mut document, &config, &sampler).unwrap();
        assert_eq!(outcome.iterations, 3);
        assert_eq!(DiagnosticDocument::live_reserved_bytes(), reserved_before);
        assert!(sampler.finish().unwrap().count > 4);
        assert_eq!(document.rows(0, 1).len(), 1);
    }

    fn make_real_document(
        name: &str,
        scenario: super::Scenario,
    ) -> (
        simpletable_lib::diagnostics::DiagnosticDocument,
        super::ChildConfig,
    ) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-{name}-{}-{unique}.csv",
            std::process::id(),
        ));
        super::ensure_fixture(&path, 50 * 1024 * 1024).unwrap();
        let document =
            simpletable_lib::diagnostics::DiagnosticDocument::open_complete(&path).unwrap();
        let config = super::ChildConfig {
            scenario,
            file: path,
            workers: 2,
            preload: false,
        };
        (document, config)
    }

    #[test]
    fn commit_fault_restores_document_and_reserved_bytes() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (mut document, config) =
            make_real_document("commit-fault", super::Scenario::CommitFail);
        let reserved_before =
            simpletable_lib::diagnostics::DiagnosticDocument::live_reserved_bytes();
        let sampler = super::SelfSampler::default();
        let outcome = super::execute_operation(&mut document, &config, &sampler).unwrap();
        assert_eq!(outcome.iterations, 1);
        assert_eq!(
            simpletable_lib::diagnostics::DiagnosticDocument::live_reserved_bytes(),
            reserved_before,
        );
        assert!(sampler.finish().unwrap().count > 0);
        assert_eq!(document.rows(0, 1).len(), 1);
    }

    #[test]
    fn read_fault_restores_document_and_reserved_bytes() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (mut document, config) = make_real_document("read-fault", super::Scenario::ReadFail);
        let reserved_before =
            simpletable_lib::diagnostics::DiagnosticDocument::live_reserved_bytes();
        let sampler = super::SelfSampler::default();
        let outcome = super::execute_operation(&mut document, &config, &sampler).unwrap();
        assert_eq!(outcome.iterations, 1);
        assert_eq!(
            simpletable_lib::diagnostics::DiagnosticDocument::live_reserved_bytes(),
            reserved_before,
        );
        assert!(sampler.finish().unwrap().count > 0);
        assert_eq!(document.rows(0, 1).len(), 1);
    }

    #[test]
    fn save_materializes_ram_once_before_disk_commit_and_uses_larger_x() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use simpletable_lib::diagnostics::LoadOptions;
        use std::sync::atomic::AtomicBool;

        let (mut document, config) = make_real_document("save", super::Scenario::Save);
        assert!(document
            .enter_memory_mode(
                &AtomicBool::new(false),
                LoadOptions {
                    workers: 2,
                    ..LoadOptions::default()
                },
                |_| {},
            )
            .unwrap());
        let sampler = super::SelfSampler::default();
        let outcome = super::execute_operation(&mut document, &config, &sampler).unwrap();
        assert_eq!(outcome.iterations, 1);
        assert!(outcome.commit_order_verified);
        // 50 MiB 真实两遍物化与写盘：各阶段计时必须来自真实区间而不是全零。
        assert!(outcome.phase_timings.count_ms > 0);
        assert!(outcome.phase_timings.ram_generation_ms > 0);
        assert!(outcome.phase_timings.disk_write_ms > 0);
        assert!(outcome.phase_timings.total_ms > 0);
        let destination = config.file.with_extension("save.csv");
        let expected_x = std::fs::metadata(&config.file)
            .unwrap()
            .len()
            .max(std::fs::metadata(&destination).unwrap().len());
        assert_eq!(outcome.x_bytes, expected_x);
        let destination_bytes = std::fs::metadata(&destination).unwrap().len();
        // `save` 只编辑单元格：夹具首行是表头（原样保留），首个数据行
        // `0000000`+LF 与 `edited`+CRLF 等长，因此这里不断言文件增大；
        // 插列导致的增大由 `column-save` 场景单独验证。
        assert!(destination_bytes >= std::fs::metadata(&config.file).unwrap().len());
        assert!(sampler.finish().unwrap().count > 0);
        // 新 `save` 场景只编辑单元格，不插列：首行首列必须变成编辑后的值。
        assert_eq!(document.rows(0, 1)[0][0], "edited");
    }

    /// 与 `run_child_with_io` 一致，在场景前进入内存模式，只测量持久操作本身。
    fn enter_memory_mode_for_test(document: &mut simpletable_lib::diagnostics::DiagnosticDocument) {
        use simpletable_lib::diagnostics::LoadOptions;
        use std::sync::atomic::AtomicBool;

        assert!(document
            .enter_memory_mode(
                &AtomicBool::new(false),
                LoadOptions {
                    workers: 2,
                    ..LoadOptions::default()
                },
                |_| {},
            )
            .unwrap());
    }

    #[test]
    fn save_retry_reuses_ram_without_second_materialization() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let (mut document, config) = make_real_document("save-retry", super::Scenario::SaveRetry);
        enter_memory_mode_for_test(&mut document);
        let sampler = super::SelfSampler::default();
        // 内部按 expected_materializations == 1 校验：第二次保存若重新物化会直接报错。
        let outcome = super::execute_operation(&mut document, &config, &sampler).unwrap();
        assert!(outcome.commit_order_verified);
        assert_eq!(outcome.iterations, 1);
        assert_eq!(document.rows(0, 1)[0][0], "edited");
        let destination = config.file.with_extension("save.csv");
        assert!(std::fs::metadata(&destination).unwrap().len() > 0);
        assert!(sampler.finish().unwrap().count > 0);
    }

    #[test]
    fn column_apply_materializes_ram_without_touching_source_file() {
        let _serial = REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let (mut document, config) =
            make_real_document("column-apply", super::Scenario::ColumnApply);
        enter_memory_mode_for_test(&mut document);
        let before = std::fs::metadata(&config.file).unwrap();
        let before_modified = before.modified().unwrap();
        let sampler = super::SelfSampler::default();
        // 内部校验零个磁盘事件、恰好一次 RAM 物化，且源文件大小与 mtime 不变。
        let outcome = super::execute_operation(&mut document, &config, &sampler).unwrap();
        assert!(outcome.commit_order_verified);
        assert!(outcome.phase_timings.count_ms > 0);
        assert!(outcome.phase_timings.ram_generation_ms > 0);
        // 只整理 RAM 的场景不得出现磁盘写入耗时。
        assert_eq!(outcome.phase_timings.disk_write_ms, 0);
        let after = std::fs::metadata(&config.file).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before_modified);
        assert_eq!(document.rows(0, 1)[0][..2], ["0000000", ""]);
        assert!(sampler.finish().unwrap().count > 0);
    }
}

#[cfg(test)]
mod protocol_tests {
    use std::io::{BufReader, Cursor, Read, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct AssertReadyInput {
        output: Arc<Mutex<Vec<u8>>>,
        inner: Cursor<Vec<u8>>,
        checked: bool,
    }

    impl Read for AssertReadyInput {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.checked {
                let output = self.output.lock().unwrap();
                let text = String::from_utf8(output.clone()).unwrap();
                assert!(text.starts_with("READY "), "GO 前必须输出 READY：{text}");
                assert!(!text.contains("RESULT"), "GO 前不得执行场景：{text}");
                self.checked = true;
            }
            self.inner.read(buffer)
        }
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "simpletable-memory-peak-protocol-{name}-{}-{unique}.csv",
            std::process::id(),
        ))
    }

    #[test]
    fn external_monitor_samples_real_pid_and_tracks_interval() {
        let monitor = super::ExternalMonitor::start(std::process::id()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(35));
        let summary = monitor.stop().unwrap();
        assert!(summary.count >= 2, "外部采样次数不足：{}", summary.count);
        assert!(summary.private_peak_bytes > 0);
        assert!(summary.max_interval_ms > 0);
    }

    #[test]
    fn child_save_ready_contains_old_ram_and_waits_for_go() {
        let _serial = super::tests::REAL_MEMORY_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use super::{ChildConfig, Scenario};

        let path = fixture_path("save");
        let file_bytes = 50 * 1024 * 1024;
        super::ensure_fixture(&path, file_bytes).unwrap();
        let config = ChildConfig {
            scenario: Scenario::Save,
            file: path,
            workers: 2,
            preload: false,
        };
        let mut output = SharedWriter::default();
        let input = AssertReadyInput {
            output: Arc::clone(&output.0),
            inner: Cursor::new(b"GO\nDONE\n".to_vec()),
            checked: false,
        };
        let mut input = BufReader::new(input);
        super::run_child_with_io(&config, &mut input, &mut output).unwrap();
        let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        let mut lines = text.lines();
        let ready: super::ReadyMessage =
            serde_json::from_str(lines.next().unwrap().strip_prefix("READY ").unwrap()).unwrap();
        assert!(ready.baseline_private_bytes > 0);
        assert!(ready.reserved_bytes >= file_bytes as usize);
        let result: super::ChildResult =
            serde_json::from_str(lines.next().unwrap().strip_prefix("RESULT ").unwrap()).unwrap();
        assert!(result.outcome.commit_order_verified);
        assert!(result.self_samples.count > 0);
        assert!(lines.next().is_none());
    }

    #[test]
    fn report_file_and_console_contain_full_hard_assertion() {
        let report = super::evaluate_peak(super::PeakInput {
            external_max_interval_ms: 17,
            ..super::PeakInput::test_defaults()
        })
        .unwrap();
        assert_eq!(report.monitor_max_interval_ms, 17);
        let path = fixture_path("report").with_extension("json");
        let mut console = Vec::new();
        super::write_report(&report, &path, &mut console).unwrap();
        let console = String::from_utf8(console).unwrap();
        assert!(console.starts_with("PEAK PASS "));
        for field in [
            "file_bytes_x",
            "allowed_peak_increment_bytes",
            "baseline_private_bytes",
            "self_peak_private_bytes",
            "external_peak_private_bytes",
            "observed_peak_private_bytes",
            "actual_peak_increment_bytes",
            "remaining_bytes",
            "excess_bytes",
            "self_sample_count",
            "external_sample_count",
            "monitor_max_interval_ms",
        ] {
            assert!(console.contains(field), "控制台缺少 {field}：{console}");
        }
        let saved: super::PeakReport =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.file_bytes_x, report.file_bytes_x);
        assert_eq!(saved.observed_peak_private_bytes, 100);
        assert!(saved.pass);
        assert_eq!(super::report_exit_code(&saved), 0);
        let mut failed = saved.clone();
        failed.pass = false;
        assert_eq!(super::report_exit_code(&failed), 1);
    }
}

#[cfg(test)]
mod parent_tests {
    #[test]
    #[ignore]
    fn process_child_entrypoint() {
        if std::env::var("SIMPLETABLE_MEMORY_PEAK_TEST_CHILD").as_deref() != Ok("1") {
            return;
        }
        let config = super::ChildConfig {
            scenario: super::Scenario::parse(
                &std::env::var("SIMPLETABLE_MEMORY_PEAK_TEST_SCENARIO").unwrap(),
            )
            .unwrap(),
            file: std::env::var("SIMPLETABLE_MEMORY_PEAK_TEST_FILE")
                .unwrap()
                .into(),
            workers: std::env::var("SIMPLETABLE_MEMORY_PEAK_TEST_WORKERS")
                .unwrap()
                .parse()
                .unwrap(),
            preload: std::env::var("SIMPLETABLE_MEMORY_PEAK_TEST_PRELOAD")
                .unwrap()
                .parse()
                .unwrap(),
        };
        super::run_child_stdio(&config).unwrap();
    }

    #[test]
    fn parent_runs_real_child_monitor_and_writes_passing_report() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "simpletable-memory-peak-parent-{}-{unique}",
            std::process::id(),
        ));
        let file = directory.join("fixture-50.csv");
        let report_path = directory.join("convert-50.json");
        assert_eq!(
            super::fixture_size_from_name(&file).unwrap(),
            50 * 1024 * 1024
        );
        let config = super::ChildConfig {
            scenario: super::Scenario::Convert,
            file: file.clone(),
            workers: 2,
            preload: false,
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "parent_tests::process_child_entrypoint",
            "--ignored",
            "--nocapture",
        ]);
        command.env("SIMPLETABLE_MEMORY_PEAK_TEST_CHILD", "1");
        command.env("SIMPLETABLE_MEMORY_PEAK_TEST_SCENARIO", "convert");
        command.env("SIMPLETABLE_MEMORY_PEAK_TEST_FILE", &file);
        command.env("SIMPLETABLE_MEMORY_PEAK_TEST_WORKERS", "2");
        command.env("SIMPLETABLE_MEMORY_PEAK_TEST_PRELOAD", "false");
        let mut console = Vec::new();
        let report =
            super::run_parent_with_command(&config, &report_path, &mut command, &mut console)
                .unwrap();
        assert!(report.pass, "真实峰值超限：{report:?}");
        let expected_profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        assert!(
            report.profile.starts_with(expected_profile),
            "报告构建档位错误：{}",
            report.profile,
        );
        assert_eq!(report.file_bytes_x, 50 * 1024 * 1024);
        assert!(report.self_sample_count > 0);
        assert!(report.external_sample_count > 0);
        assert!(report.monitor_max_interval_ms > 0);
        assert!(String::from_utf8(console)
            .unwrap()
            .starts_with("PEAK PASS "));
        let saved: super::PeakReport =
            serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
        assert!(saved.pass);
    }
}
