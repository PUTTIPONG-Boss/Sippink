<#
Removes what scripts\install.ps1 added: the program folder, its shortcuts
and the Settings > Apps entry. Downloads are never touched.

  -KeepTools  keep yt-dlp / ffmpeg / deno in %APPDATA%\sippink
#>
param([switch]$KeepTools)

$name = "Sippink"
$dir = Join-Path $env:LOCALAPPDATA "Programs\$name"
$target = Join-Path $dir "$name.exe"

Get-Process sippink -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $target } |
    Stop-Process -Force
Start-Sleep -Milliseconds 300

$links = @(
    (Join-Path ([Environment]::GetFolderPath("Programs")) "$name.lnk"),
    (Join-Path ([Environment]::GetFolderPath("Desktop")) "$name.lnk")
)
foreach ($l in $links) {
    # Only shortcuts that point at the installed copy.
    if ((Test-Path $l) -and (New-Object -ComObject WScript.Shell).CreateShortcut($l).TargetPath -eq $target) {
        Remove-Item $l -Force
    }
}
Remove-Item "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$name" -Recurse -Force -ErrorAction SilentlyContinue
if (-not $KeepTools) {
    Remove-Item (Join-Path $env:APPDATA "sippink") -Recurse -Force -ErrorAction SilentlyContinue
}
# This script lives in $dir; PowerShell has already read it, so it can go too.
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "Uninstalled $name"
