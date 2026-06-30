use admin_common::{
    close_pipe, connect_pipe_server, create_pipe_server, read_pipe_message, write_pipe_message,
    AdminOperation, AdminRequest, AdminResponse,
};
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::fs::{create_dir_all, read_to_string, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;
use tokio::runtime::Runtime;
use tokio::time::timeout;
use windows::Win32::Foundation::HANDLE;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const SERVICE_NAME: &str = "AdminPowerShellMcpBroker";
const DATA_DIR: &str = r"C:\ProgramData\admin-powershell-mcp";
const LOG_PATH: &str = r"C:\ProgramData\admin-powershell-mcp\broker.log";
const POLICY_PATH: &str = r"C:\ProgramData\admin-powershell-mcp\policy.toml";
const MAX_CONCURRENT_REQUESTS: usize = 8;

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--ping") => ping(),
        Some("--serve") => serve(Arc::new(AtomicBool::new(false))),
        Some("--service") => service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("failed to start service dispatcher"),
        _ => {
            eprintln!("usage: admin-broker --serve | --service | --ping");
            Ok(())
        }
    }
}

fn ping() -> Result<()> {
    let resp = admin_common::send_request(&AdminRequest {
        operation: AdminOperation::Ping,
        reason: "local broker smoke check".to_string(),
    })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

fn serve(stop: Arc<AtomicBool>) -> Result<()> {
    log_line("admin-broker listening on \\\\.\\pipe\\admin-powershell-mcp");
    let limiter = Arc::new(ConcurrencyLimiter::new(MAX_CONCURRENT_REQUESTS));
    while !stop.load(Ordering::Relaxed) {
        let handle = create_pipe_server()?;
        connect_pipe_server(handle)?;
        let permit = limiter.acquire();
        let raw_handle = handle.0 as usize;
        std::thread::spawn(move || {
            let _permit = permit;
            let handle = HANDLE(raw_handle as _);
            if let Err(err) = handle_one(handle) {
                log_line(&format!("request failed: {err:#}"));
                close_pipe(handle);
            }
        });
    }
    log_line("admin-broker stopped");
    Ok(())
}

fn service_main(_args: Vec<std::ffi::OsString>) {
    if let Err(err) = run_service() {
        log_line(&format!("service error: {err:#}"));
    }
}

fn run_service() -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = Arc::clone(&stop);
    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop => {
                stop_for_handler.store(true, Ordering::Relaxed);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    let result = serve(stop);

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    result
}

fn handle_one(handle: HANDLE) -> Result<()> {
    let bytes = read_pipe_message(handle)?;
    let req: AdminRequest = serde_json::from_slice(&bytes).context("invalid request json")?;
    let start = Instant::now();
    log_line(&format!(
        "operation={:?} reason={}",
        req.operation, req.reason
    ));
    let resp = handle_request(req);
    log_line(&format!(
        "done ok={} exit_code={:?} elapsed={:?}",
        resp.ok,
        resp.exit_code,
        start.elapsed()
    ));
    write_pipe_message(handle, &serde_json::to_vec(&resp)?)?;
    close_pipe(handle);
    Ok(())
}

struct ConcurrencyLimiter {
    max: usize,
    active: Mutex<usize>,
    changed: Condvar,
}

impl ConcurrencyLimiter {
    fn new(max: usize) -> Self {
        Self {
            max,
            active: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn acquire(self: &Arc<Self>) -> Permit {
        let mut active = self.active.lock().expect("limiter poisoned");
        while *active >= self.max {
            active = self.changed.wait(active).expect("limiter poisoned");
        }
        *active += 1;
        Permit {
            limiter: Arc::clone(self),
        }
    }
}

struct Permit {
    limiter: Arc<ConcurrencyLimiter>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut active = self.limiter.active.lock().expect("limiter poisoned");
        *active -= 1;
        self.limiter.changed.notify_one();
    }
}

fn handle_request(req: AdminRequest) -> AdminResponse {
    if req.reason.trim().is_empty() {
        return AdminResponse::err("reason is required");
    }
    match run(req.operation) {
        Ok(resp) => resp,
        Err(err) => AdminResponse::err(format!("{err:#}")),
    }
}

fn run(op: AdminOperation) -> Result<AdminResponse> {
    match op {
        AdminOperation::Ping => Ok(AdminResponse::ok("pong")),
        AdminOperation::GetStatus => ps("$PSVersionTable.PSVersion.ToString()"),
        AdminOperation::RunCommand {
            command,
            cwd,
            timeout_seconds,
            max_output_bytes,
        } => run_command(&command, cwd.as_deref(), timeout_seconds, max_output_bytes),
    }
}

fn run_command(
    command: &str,
    cwd: Option<&str>,
    timeout_seconds: Option<u64>,
    max_output_bytes: Option<usize>,
) -> Result<AdminResponse> {
    validate_command(command)?;
    let timeout = command_timeout(timeout_seconds)?;
    let max_output = command_max_output(max_output_bytes)?;
    match Runtime::new()?.block_on(run_command_async(command, cwd, timeout))? {
        Some(output) => Ok(response_with_limit(output, max_output)),
        None => Ok(AdminResponse {
            ok: false,
            exit_code: None,
            stdout: String::new(),
            stderr: format!("command timed out after {} seconds", timeout.as_secs()),
        }),
    }
}

async fn run_command_async(
    command: &str,
    cwd: Option<&str>,
    timeout_duration: Duration,
) -> Result<Option<Output>> {
    let mut cmd = TokioCommand::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        command,
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    if let Some(cwd) = cwd.filter(|s| !s.trim().is_empty()) {
        cmd.current_dir(Path::new(cwd));
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to run command: {command}"))?;
    match timeout(timeout_duration, child.wait_with_output()).await {
        Ok(output) => output.map(Some).context("read command output"),
        Err(_) => Ok(None),
    }
}

fn command_max_output(requested: Option<usize>) -> Result<usize> {
    let policy = load_policy()?;
    let max = policy
        .max_output_bytes
        .unwrap_or(1_000_000)
        .clamp(1, 100_000_000);
    let default = policy.default_output_bytes.unwrap_or(200_000).clamp(1, max);
    Ok(requested.unwrap_or(default).clamp(1, max))
}

fn command_timeout(requested: Option<u64>) -> Result<Duration> {
    let policy = load_policy()?;
    let max = policy.max_timeout_seconds.unwrap_or(600).clamp(1, 86_400);
    let default = policy.default_timeout_seconds.unwrap_or(120).clamp(1, max);
    let seconds = requested.unwrap_or(default).clamp(1, max);
    Ok(Duration::from_secs(seconds))
}

fn validate_command(command: &str) -> Result<()> {
    let normalized = normalize_command(command);
    if normalized.is_empty() {
        return Err(anyhow!("command is required"));
    }
    if policy_allows(&normalized)? || default_prefix_allows(&normalized) {
        return Ok(());
    }
    Err(anyhow!("command is not allowed by whitelist: {command}"))
}

fn policy_allows(command: &str) -> Result<bool> {
    let policy = load_policy()?;
    for pattern in policy.allowed_command_regexes {
        let re = Regex::new(&pattern).with_context(|| format!("invalid regex: {pattern}"))?;
        if re.is_match(command) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_policy() -> Result<Policy> {
    let Ok(text) = read_to_string(POLICY_PATH) else {
        return Ok(Policy::default());
    };
    toml::from_str(&text).context("invalid policy.toml")
}

fn default_prefix_allows(command: &str) -> bool {
    ALLOWED_COMMAND_PREFIXES
        .iter()
        .any(|prefix| command == *prefix || command.starts_with(&format!("{prefix} ")))
}

#[derive(Debug, Default, Deserialize)]
struct Policy {
    #[serde(default)]
    allowed_command_regexes: Vec<String>,
    default_timeout_seconds: Option<u64>,
    max_timeout_seconds: Option<u64>,
    default_output_bytes: Option<usize>,
    max_output_bytes: Option<usize>,
}

fn normalize_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

const ALLOWED_COMMAND_PREFIXES: &[&str] = &[
    "chkdsk c: /scan",
    "dism /online /cleanup-image /restorehealth",
    "dism /online /cleanup-image /scanhealth",
    "fsutil volume diskfree",
    "get-computerinfo",
    "get-eventlog",
    "get-process",
    "get-service",
    "get-winevent",
    "ipconfig /all",
    "ipconfig /displaydns",
    "ipconfig /flushdns",
    "restart-service",
    "sc.exe qc",
    "sc.exe query",
    "sc.exe start",
    "sc.exe stop",
    "sfc /scannow",
    "start-service",
    "stop-service",
    "wevtutil gli",
    "wevtutil qe",
    "winget list",
    "winget show",
    "winget upgrade",
];

fn cmd(program: &str, args: &[&str]) -> Result<AdminResponse> {
    let out = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    Ok(response(out))
}

fn ps(script: &str) -> Result<AdminResponse> {
    cmd(
        "powershell.exe",
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ],
    )
}

fn response(out: std::process::Output) -> AdminResponse {
    AdminResponse {
        ok: out.status.success(),
        exit_code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn response_with_limit(out: Output, max_bytes: usize) -> AdminResponse {
    AdminResponse {
        ok: out.status.success(),
        exit_code: out.status.code(),
        stdout: limited_lossy(&out.stdout, max_bytes, "stdout"),
        stderr: limited_lossy(&out.stderr, max_bytes, "stderr"),
    }
}

fn limited_lossy(bytes: &[u8], max_bytes: usize, name: &str) -> String {
    if bytes.len() <= max_bytes {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from_utf8_lossy(&bytes[..max_bytes]).into_owned();
    out.push_str(&format!(
        "\n\n[admin-powershell-mcp: {name} truncated at {max_bytes} of {} bytes]",
        bytes.len()
    ));
    out
}

fn log_line(message: &str) {
    let _ = create_dir_all(DATA_DIR);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
        let _ = writeln!(file, "{:?} {}", std::time::SystemTime::now(), message);
    }
    eprintln!("{message}");
}
