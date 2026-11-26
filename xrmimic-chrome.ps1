# Start Chrome with xrmimic as its OpenXR runtime, on a separate profile.
# The runtime is chosen through XR_RUNTIME_JSON for this Chrome only; the
# system's active runtime changes only if you pick one in the headset window.
#
#   .\xrmimic-chrome.ps1 [-Url <url>] [-Chrome <path>] [-UserDataDir <dir>] [-NoSandbox]
#
# Without -Url it opens the XRMimic test page (tools\testpage), served on
# http://localhost:8771/ by xrmimic-launch until this Chrome exits.
#
# Chrome keeps its sandbox: its XR process runs in an AppContainer, so the
# runtime copy, manifest and log live in %LOCALAPPDATA%\xrmimic\runtime, the one
# folder app containers are granted access to.
param(
    [string]$Url = "",
    [int]$TestPagePort = 8771,
    [string]$Chrome = "C:\Program Files\Google\Chrome\Application\chrome.exe",
    [string]$UserDataDir = (Join-Path $env:LOCALAPPDATA "xrmimic\chrome-profile"),
    [switch]$NoSandbox
)

$dll = Join-Path $PSScriptRoot "target\release\xrmimic_runtime.dll"
$helper = Join-Path $PSScriptRoot "target\release\xrmimic-launch.exe"
if (-not (Test-Path $dll) -or -not (Test-Path $helper)) {
    Write-Error "Not built: run 'cargo build --release' in $PSScriptRoot"
    exit 1
}

$running = Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object {
    $_.CommandLine -like "*--user-data-dir=*$UserDataDir*" -and $_.CommandLine -notlike "*--type=*"
}
if ($running) {
    Write-Error "Chrome is already running on this profile (pid $($running.ProcessId -join ', ')). Close that window first: a second launch would reuse it with the old runtime."
    exit 1
}
New-Item -ItemType Directory -Force $UserDataDir | Out-Null

# Chrome keeps the runtime DLL loaded while it runs, so each launch gets its
# own copy and cargo can rebuild target\release meanwhile.
$runtimeRoot = Join-Path $env:LOCALAPPDATA "xrmimic\runtime"
New-Item -ItemType Directory -Force $runtimeRoot | Out-Null
# ALL APPLICATION PACKAGES may read the runtime and write its log here (only here).
icacls $runtimeRoot /grant "*S-1-15-2-1:(OI)(CI)(M)" | Out-Null
Get-ChildItem $runtimeRoot -Directory | ForEach-Object {
    try { Remove-Item -Recurse -Force $_.FullName -ErrorAction Stop } catch {}
}
$runtimeDir = Join-Path $runtimeRoot (Get-Date -Format "yyyyMMdd-HHmmss")
New-Item -ItemType Directory -Force $runtimeDir | Out-Null
Copy-Item $dll $runtimeDir
$manifest = Join-Path $runtimeDir "xrmimic.json"
@'
{
    "file_format_version": "1.0.0",
    "runtime": { "name": "XRMimic", "library_path": "./xrmimic_runtime.dll" }
}
'@ | Set-Content -Encoding ascii $manifest
$log = Join-Path $runtimeDir "xrmimic.log"

# Runtimes the sandboxed window cannot discover itself: SteamVR (its path is in
# the user's AppData) and this checkout's xrmimic.
$extra = @((Join-Path $PSScriptRoot "xrmimic.json"))
$vrpath = Join-Path $env:LOCALAPPDATA "openvr\openvrpaths.vrpath"
if (Test-Path $vrpath) {
    $steam = (Get-Content -Raw $vrpath | ConvertFrom-Json).runtime | Select-Object -First 1
    if ($steam -and (Test-Path (Join-Path $steam "steamxr_win64.json"))) {
        $extra += (Join-Path $steam "steamxr_win64.json")
    }
}

$env:XR_RUNTIME_JSON = $manifest
$env:XRMIMIC_LOG = $log
# The Device tab's settings, kept between launches (readable inside the sandbox).
$env:XRMIMIC_DEVICE_FILE = Join-Path $runtimeRoot "device.ini"
$env:XRMIMIC_EXTRA_RUNTIMES = $extra -join ";"
# Switch requests from the window go through this folder to the helper below.
$env:XRMIMIC_SWITCH_DIR = $runtimeDir
$chromeArgs = @(
    "--user-data-dir=`"$UserDataDir`"",
    "--no-first-run",
    "--no-default-browser-check"
)
if ($NoSandbox) { $chromeArgs += "--no-sandbox" }

# No -Url: the XRMimic test page, served by the helper (unless something already
# serves that port, e.g. an earlier launch's server).
$server = $null
if (-not $Url) {
    $Url = "http://localhost:$TestPagePort/"
    $busy = Get-NetTCPConnection -LocalPort $TestPagePort -State Listen -ErrorAction SilentlyContinue
    if (-not $busy) {
        $page = Join-Path $PSScriptRoot "tools\testpage"
        $server = Start-Process -FilePath $helper -ArgumentList @("serve", "`"$page`"", $TestPagePort) -WindowStyle Hidden -PassThru
        for ($i = 0; $i -lt 40; $i++) {
            if (Get-NetTCPConnection -LocalPort $TestPagePort -State Listen -ErrorAction SilentlyContinue) { break }
            Start-Sleep -Milliseconds 50
        }
    }
}
$chromeArgs += $Url
$p = Start-Process -FilePath $Chrome -ArgumentList $chromeArgs -PassThru

# Outside the sandbox: performs runtime switches (with the Windows prompt)
# for the headset window, and exits with this Chrome (stopping the page server).
$watchArgs = @("watch", "`"$runtimeDir`"", $p.Id)
if ($server) { $watchArgs += $server.Id }
$h = Start-Process -FilePath $helper -ArgumentList $watchArgs -WindowStyle Hidden -PassThru

Write-Host "Chrome pid $($p.Id), switch helper pid $($h.Id)$(if ($server) { ", page server pid $($server.Id)" })"
Write-Host "Page:    $Url"
Write-Host "Runtime: $manifest"
Write-Host "Log:     $log"
