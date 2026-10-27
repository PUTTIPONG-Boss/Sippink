<#
Installs Sippink for the current user (no admin needed):

  - copies Sippink.exe to %LOCALAPPDATA%\Programs\Sippink
  - adds Start Menu and Desktop shortcuts (so Windows search finds it)
  - registers it under Settings > Apps, with an Uninstall button

  irm https://raw.githubusercontent.com/PUTTIPONG-Boss/Sippink/main/scripts/install.ps1 | iex
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1            # latest GitHub release
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Exe path  # a local build

Running it again updates an existing install.
#>
param(
    [string]$Exe,
    [switch]$NoDesktop
)
$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$repo = "https://github.com/PUTTIPONG-Boss/Sippink"

$name = "Sippink"
$dir = Join-Path $env:LOCALAPPDATA "Programs\$name"
$target = Join-Path $dir "$name.exe"
$startMenu = Join-Path ([Environment]::GetFolderPath("Programs")) "$name.lnk"
$desktop = Join-Path ([Environment]::GetFolderPath("Desktop")) "$name.lnk"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$name"

# A running copy keeps the exe locked.
Get-Process sippink -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $target } |
    Stop-Process -Force
Start-Sleep -Milliseconds 300

New-Item -ItemType Directory -Force $dir | Out-Null
if ($Exe) {
    Copy-Item -LiteralPath $Exe -Destination $target -Force
} else {
    # releases/latest redirects to .../tag/vX.Y.Z; assets are named after it.
    $req = [Net.WebRequest]::Create("$repo/releases/latest")
    $req.AllowAutoRedirect = $false
    $resp = $req.GetResponse(); $tag = $resp.Headers["Location"].Split("/")[-1]; $resp.Close()
    $url = "$repo/releases/download/$tag/sippink-$tag-windows-x64.exe"
    Write-Host "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $target -UseBasicParsing
}
# Next to this script in a clone; fetched when run through `irm | iex`.
$uninstall = if ($PSScriptRoot) { Join-Path $PSScriptRoot "uninstall.ps1" } else { "" }
if ($uninstall -and (Test-Path $uninstall)) {
    Copy-Item -LiteralPath $uninstall -Destination $dir -Force
} else {
    Invoke-WebRequest -Uri "https://raw.githubusercontent.com/PUTTIPONG-Boss/Sippink/main/scripts/uninstall.ps1" `
        -OutFile (Join-Path $dir "uninstall.ps1") -UseBasicParsing
}

function New-Shortcut([string]$path) {
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
    $s.TargetPath = $target
    $s.WorkingDirectory = $dir
    $s.IconLocation = "$target,0"
    $s.Description = "Paste a link, sip the media"
    $s.Save()
}
New-Shortcut $startMenu
if (-not $NoDesktop) { New-Shortcut $desktop }

$version = (Get-Item $target).VersionInfo.ProductVersion
New-Item -Force $uninstallKey | Out-Null
$props = @{
    DisplayName     = $name
    DisplayVersion  = $version
    Publisher       = "PUTTIPONG-Boss"
    DisplayIcon     = "$target,0"
    InstallLocation = $dir
    URLInfoAbout    = "https://github.com/PUTTIPONG-Boss/Sippink"
    UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$dir\uninstall.ps1`""
}
foreach ($k in $props.Keys) { Set-ItemProperty $uninstallKey -Name $k -Value $props[$k] }
Set-ItemProperty $uninstallKey -Name NoModify -Value 1 -Type DWord
Set-ItemProperty $uninstallKey -Name NoRepair -Value 1 -Type DWord
Set-ItemProperty $uninstallKey -Name EstimatedSize -Value ([int]((Get-Item $target).Length / 1KB)) -Type DWord

Write-Host "Installed $name $version to $dir"
