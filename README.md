# admin-powershell-mcp

Windows 管理员维护操作的最小 MCP 桥接工具。

## 构建

```powershell
cargo build
```

## 本地运行

调试时，可以先在管理员 PowerShell 里前台启动 broker：

```powershell
.\target\debug\admin-broker.exe --serve
```

普通终端里可以检查 named pipe 是否通：

```powershell
.\target\debug\admin-broker.exe --ping
```

## 安装为 Windows Service

在管理员 PowerShell 里运行：

```powershell
cargo build
.\scripts\install-service.ps1
```

卸载服务：

```powershell
.\scripts\uninstall-service.ps1
```

如果之前安装过旧版 `admin-powershell`，先在管理员 PowerShell 清理旧服务和旧文件：

```powershell
sc.exe stop AdminPowerShellBroker
sc.exe delete AdminPowerShellBroker
Remove-Item -Recurse -Force "C:\Program Files\admin-powershell" -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force "C:\ProgramData\admin-powershell" -ErrorAction SilentlyContinue
```

服务名是 `AdminPowerShellMcpBroker`。日志写到：

```text
C:\ProgramData\admin-powershell-mcp\broker.log
```

安装脚本会复制：

```text
C:\Program Files\admin-powershell-mcp\admin-broker.exe
C:\Program Files\admin-powershell-mcp\admin-mcp-server.exe
```

配置和日志仍放在：

```text
C:\ProgramData\admin-powershell-mcp
```

## 命令白名单

`run_command` 会先读取：

```text
C:\ProgramData\admin-powershell-mcp\policy.toml
```

格式：

```toml
allowed_command_regexes = [
  "^ipconfig /flushdns$",
  "^get-service( .*)?$",
  "^restart-service [a-z0-9_.-]+$",
]
default_timeout_seconds = 120
max_timeout_seconds = 600
default_output_bytes = 200000
max_output_bytes = 1000000
```

正则匹配的是规范化后的命令：空白会压缩，大小写会转小写。`policy.toml` 不存在或没有匹配时，会回退到内置前缀白名单。示例文件在 [config/policy.example.toml](D:\projects\admin-powershell-mcp\config\policy.example.toml)。

## Codex MCP 配置

把下面配置加入 Codex 的 `config.toml`：

```toml
[mcp_servers.admin_powershell_mcp]
command = "C:\\Program Files\\admin-powershell-mcp\\admin-mcp-server.exe"
startup_timeout_sec = 10
tool_timeout_sec = 300
default_tools_approval_mode = "auto"
```

## 当前工具

- `ping`：检查 broker 是否可用
- `get_status`：返回 broker/PowerShell 状态
- `run_command`：像 shell 一样传入完整命令字符串，但只执行白名单命令

`run_command` 示例：

```text
ipconfig /flushdns
Get-Service Spooler
Restart-Service Spooler
winget upgrade Microsoft.PowerShell
```

`run_command` 可以传 `timeout_seconds` 和 `max_output_bytes`。未传时使用 `policy.toml` 的默认值：超时默认 120 秒，输出默认 200000 字节；最大值分别由 `max_timeout_seconds` 和 `max_output_bytes` 限制。

## 当前限制

服务停止时如果正在等待 named pipe 连接，可能需要一次连接或强制停止后才退出；后面需要时再改成 overlapped IO。broker 会并发处理最多 8 个请求，命令执行使用 Tokio async process 和 timeout。
