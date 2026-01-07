# =================配置区域=================
# 修改 尚未配置 为任意密码
$env:API_TOKEN = "kuriko"

# =================逻辑区域=================
if ($env:API_TOKEN -eq "尚未配置") {
    Write-Host "错误：请用记事本编辑 config.ps1 文件，修改 API_TOKEN！" -ForegroundColor Red
    Read-Host "按回车退出..."
    exit
}

$ExePath = Join-Path $PSScriptRoot "SteamCurrentGameClient.exe"

if (-not (Test-Path $ExePath)) {
    Write-Host "错误：找不到客户端程序！" -ForegroundColor Red
    Read-Host "按回车退出..."
    exit
}

Write-Host "正在启动... Token: $env:API_TOKEN" -ForegroundColor Green
& $ExePath