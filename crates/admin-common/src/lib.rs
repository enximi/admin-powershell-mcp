use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub const PIPE_NAME: &str = r"\\.\pipe\admin-powershell-mcp";
const BUFFER_SIZE: u32 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRequest {
    pub operation: AdminOperation,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminOperation {
    Ping,
    GetStatus,
    RunCommand {
        command: String,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
        max_output_bytes: Option<usize>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminResponse {
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl AdminResponse {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            ok: true,
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            exit_code: None,
            stdout: String::new(),
            stderr: message.into(),
        }
    }
}

pub fn read_pipe_message(handle: HANDLE) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; BUFFER_SIZE as usize];
    loop {
        let mut read = 0;
        if unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) }.is_err() {
            break;
        }
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
        if read < BUFFER_SIZE {
            break;
        }
    }
    Ok(out)
}

pub fn write_pipe_message(handle: HANDLE, bytes: &[u8]) -> Result<()> {
    let mut written_total = 0usize;
    while written_total < bytes.len() {
        let chunk = &bytes[written_total..bytes.len().min(written_total + BUFFER_SIZE as usize)];
        let mut written = 0;
        unsafe { WriteFile(handle, Some(chunk), Some(&mut written), None) }
            .context("WriteFile failed")?;
        written_total += written as usize;
    }
    Ok(())
}

pub fn create_pipe_server() -> Result<HANDLE> {
    let name = wide(PIPE_NAME);
    let mut pipe_security = PipeSecurity::new()?;
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            Some(pipe_security.attributes()),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(anyhow!("CreateNamedPipeW failed: {:?}", unsafe {
            GetLastError()
        }));
    }
    Ok(handle)
}

struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    fn new() -> Result<Self> {
        // Administrators/System full access, authenticated users read/write.
        let sddl = wide("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)");
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("invalid pipe security descriptor")?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    fn attributes(&mut self) -> *const SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

pub fn connect_pipe_server(handle: HANDLE) -> Result<()> {
    if let Err(err) = unsafe { ConnectNamedPipe(handle, None) } {
        if unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            return Err(err).context("ConnectNamedPipe failed");
        }
    }
    Ok(())
}

pub fn close_pipe(handle: HANDLE) {
    unsafe {
        DisconnectNamedPipe(handle).ok();
        CloseHandle(handle).ok();
    }
}

pub fn send_request(req: &AdminRequest) -> Result<AdminResponse> {
    let name = wide(PIPE_NAME);
    loop {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                Default::default(),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        if let Ok(handle) = handle {
            let bytes = serde_json::to_vec(req)?;
            write_pipe_message(handle, &bytes)?;
            let response = read_pipe_message(handle)?;
            unsafe { CloseHandle(handle).ok() };
            return serde_json::from_slice(&response).context("invalid broker response");
        }
        let err = unsafe { GetLastError() };
        if err != ERROR_PIPE_BUSY {
            return Err(anyhow!("open pipe failed: {err:?}"));
        }
        let waited = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 5_000) };
        if !waited.as_bool() {
            return Err(anyhow!("broker pipe is busy"));
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(once(0)).collect()
}
