$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$BuiltBrokerExe = Join-Path $Root "target\debug\admin-broker.exe"
$BuiltMcpExe = Join-Path $Root "target\debug\admin-mcp-server.exe"
$ServiceName = "AdminPowerShellMcpBroker"
$BinDir = "C:\Program Files\admin-powershell-mcp"
$DataDir = "C:\ProgramData\admin-powershell-mcp"
$BrokerExe = Join-Path $BinDir "admin-broker.exe"
$McpExe = Join-Path $BinDir "admin-mcp-server.exe"

if (-not (Test-Path $BuiltBrokerExe)) {
    throw "找不到 $BuiltBrokerExe。请先运行 cargo build。"
}
if (-not (Test-Path $BuiltMcpExe)) {
    throw "找不到 $BuiltMcpExe。请先运行 cargo build。"
}

sc.exe stop $ServiceName | Out-Null
sc.exe delete $ServiceName | Out-Null
Start-Sleep -Seconds 1

New-Item -ItemType Directory -Force $BinDir | Out-Null
New-Item -ItemType Directory -Force $DataDir | Out-Null
Copy-Item -Force $BuiltBrokerExe $BrokerExe
Copy-Item -Force $BuiltMcpExe $McpExe

sc.exe create $ServiceName binPath= "`"$BrokerExe`" --service" start= auto DisplayName= "Admin PowerShell MCP Broker" | Out-Null
sc.exe description $ServiceName "Local named-pipe broker for whitelisted Windows admin maintenance operations exposed through MCP." | Out-Null
sc.exe start $ServiceName | Out-Null

Write-Host "已安装并启动 $ServiceName。"
Write-Host "MCP server 已复制到 $McpExe。"
