$ErrorActionPreference = "Stop"

$ServiceName = "AdminPowerShellMcpBroker"

sc.exe stop $ServiceName | Out-Null
sc.exe delete $ServiceName | Out-Null

Remove-Item -Force "C:\Program Files\admin-powershell-mcp\admin-broker.exe" -ErrorAction SilentlyContinue
Remove-Item -Force "C:\Program Files\admin-powershell-mcp\admin-mcp-server.exe" -ErrorAction SilentlyContinue

Write-Host "已卸载 $ServiceName。"
