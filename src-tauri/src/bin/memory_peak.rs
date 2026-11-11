use std::io::Write;
use std::path::{Path, PathBuf};

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
            Self::CancelLoop => "cancel-loop",
        }
    }
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
    sample_current_process, sample_process, DiagnosticDocument, LoadEvent, LoadOptions,
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

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct OperationOutcome {
    x_bytes: u64,
    iterations: u64,
    old_ram_released_before_new_reserve: bool,
}

fn enter_with_optional_cancel(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    cancel_after_quarter: bool,
    fault: simpletable_lib::diagnostics::LoadFault,
    sampler: &SelfSampler,
) -> Result<bool, String> {
    let cancel = AtomicBool::new(false);
    let options = LoadOptions {
        workers: config.workers,
        fault,
        ..LoadOptions::default()
    };
    document.enter_memory_mode(&cancel, options, |event| {
        sampler.sample();
        if cancel_after_quarter {
            if let LoadEvent::Progress { done, total } = event {
                if total > 0 && done > total / 4 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
    })
}

fn expected_fault_recovers(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    sampler: &SelfSampler,
    fault: simpletable_lib::diagnostics::LoadFault,
    expected_error: &str,
) -> Result<(), String> {
    let reserved_before = DiagnosticDocument::live_reserved_bytes();
    let error = match enter_with_optional_cancel(document, config, false, fault, sampler) {
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

fn execute_save(
    document: &mut DiagnosticDocument,
    config: &ChildConfig,
    sampler: &SelfSampler,
    source_bytes: u64,
) -> Result<OperationOutcome, String> {
    use simpletable_lib::diagnostics::{LoadFault, SaveRamEvent, SaveRamPhase};

    let destination = config.file.with_extension("save.csv");
    let phases = Mutex::new(Vec::with_capacity(16));
    document.set_cell(0, 0, "9".into())?;
    document.save_with_ram_observer(
        &destination,
        LoadOptions {
            workers: config.workers,
            fault: LoadFault::None,
            ..LoadOptions::default()
        },
        |event| {
            sampler.sample();
            if let SaveRamEvent::Phase(phase) = event {
                phases.lock().unwrap().push(phase);
            }
        },
    )?;
    let phases = phases
        .into_inner()
        .map_err(|_| "保存事件锁已中毒".to_string())?;
    let released = phases
        .iter()
        .position(|phase| matches!(phase, SaveRamPhase::OldRamReleased { .. }));
    let reserve = phases
        .iter()
        .position(|phase| matches!(phase, SaveRamPhase::NewReserveBegin { .. }));
    let old_ram_released_before_new_reserve = match (released, reserve) {
        (Some(released), Some(reserve)) => released < reserve,
        _ => false,
    };
    if !old_ram_released_before_new_reserve {
        return Err(format!("保存 RAM 释放顺序错误：{phases:?}"));
    }
    let destination_bytes = std::fs::metadata(&destination)
        .map_err(|error| format!("读取保存后文件大小失败：{error}"))?
        .len();
    Ok(OperationOutcome {
        x_bytes: source_bytes.max(destination_bytes),
        iterations: 1,
        old_ram_released_before_new_reserve,
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
            if !enter_with_optional_cancel(
                document,
                config,
                false,
                simpletable_lib::diagnostics::LoadFault::None,
                sampler,
            )? {
                return Err("普通转换未进入内存模式".into());
            }
            Ok(OperationOutcome {
                x_bytes,
                iterations: 1,
                old_ram_released_before_new_reserve: false,
            })
        }
        Scenario::Cancel => {
            let reserved_before = DiagnosticDocument::live_reserved_bytes();
            if enter_with_optional_cancel(
                document,
                config,
                true,
                simpletable_lib::diagnostics::LoadFault::None,
                sampler,
            )? {
                return Err("取消场景意外进入内存模式".into());
            }
            let reserved_after = DiagnosticDocument::live_reserved_bytes();
            if reserved_after != reserved_before {
                return Err(format!(
                    "取消恢复遗留区域：之前 {reserved_before}，之后 {reserved_after}"
                ));
            }
            Ok(OperationOutcome {
                x_bytes,
                iterations: 1,
                old_ram_released_before_new_reserve: false,
            })
        }
        Scenario::CancelLoop => {
            let reserved_before = DiagnosticDocument::live_reserved_bytes();
            for iteration in 0..3 {
                if enter_with_optional_cancel(
                    document,
                    config,
                    true,
                    simpletable_lib::diagnostics::LoadFault::None,
                    sampler,
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
            Ok(OperationOutcome {
                x_bytes,
                iterations: 3,
                old_ram_released_before_new_reserve: false,
            })
        }
        Scenario::CommitFail => {
            expected_fault_recovers(
                document,
                config,
                sampler,
                simpletable_lib::diagnostics::LoadFault::CommitAt(0),
                "注入提交失败",
            )?;
            Ok(OperationOutcome {
                x_bytes,
                iterations: 1,
                old_ram_released_before_new_reserve: false,
            })
        }
        Scenario::ReadFail => {
            expected_fault_recovers(
                document,
                config,
                sampler,
                simpletable_lib::diagnostics::LoadFault::ReadAfter(0),
                "注入读取失败",
            )?;
            Ok(OperationOutcome {
                x_bytes,
                iterations: 1,
                old_ram_released_before_new_reserve: false,
            })
        }
        Scenario::Save => execute_save(document, config, sampler, x_bytes),
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

    if config.scenario == Scenario::Save {
        let warmup_sampler = SelfSampler::default();
        if !enter_with_optional_cancel(
            &mut document,
            config,
            false,
            simpletable_lib::diagnostics::LoadFault::None,
            &warmup_sampler,
        )? {
            return Err("保存场景未能预先进入内存模式".into());
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
    pub pass: bool,
}

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
        pass: actual <= allowed,
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
    })?;
    write_report(&report, report_path, output)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    pub(super) static REAL_MEMORY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    use super::{evaluate_peak, PeakInput};

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
        document.set_cell(0, 0, "9".into()).unwrap();
        let events = Mutex::new(Vec::new());
        document
            .save_with_ram_observer(&destination, LoadOptions::default(), |event| {
                events.lock().unwrap().push(event);
            })
            .unwrap();
        assert!(destination.exists());
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
    fn save_releases_old_ram_before_new_reserve_and_uses_larger_x() {
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
        assert!(outcome.old_ram_released_before_new_reserve);
        let destination = config.file.with_extension("save.csv");
        let expected_x = std::fs::metadata(&config.file)
            .unwrap()
            .len()
            .max(std::fs::metadata(destination).unwrap().len());
        assert_eq!(outcome.x_bytes, expected_x);
        assert!(sampler.finish().unwrap().count > 0);
        assert_eq!(document.rows(0, 1)[0][0], "9");
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
        assert!(result.outcome.old_ram_released_before_new_reserve);
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
