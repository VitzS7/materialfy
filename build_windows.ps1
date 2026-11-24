#Requires -Version 7.0
<#
.SYNOPSIS
    Builds Materialfy (Rust + Qt 6.12) for Windows: the exe, a portable folder and an Inno Setup installer.

.DESCRIPTION
    1. Finds Qt 6.12 MSVC (qmake6.exe, windeployqt.exe) and sets QMAKE/PATH for cxx-qt.
    2. cargo build --release -p materialfy (or -CargoProfile debug).
    3. Assembles target\dist\materialfy\: materialfy.exe, qml\ (src\materialfy\qml), assets\ (fonts,
       icons), Qt through windeployqt --qmldir, the MSVC runtime.
    4. Builds the installer with Inno Setup -> target\dist\Materialfy-<version>-Setup.exe.

    Audio is played by librespot inside the exe (cpal/WASAPI opens the device); there are no external player DLLs.

.PARAMETER QtDir
    Qt prefix (the directory containing bin\qmake6.exe). Default: $env:QTDIR, the directory of $env:QMAKE,
    C:\Qt\6.12.0\msvc2022_64, ~\Qt\6.12.0\msvc2022_64, ~\Qt\6.12.0.

.PARAMETER Version
    Installer version. Default: [workspace.package] version from Cargo.toml.

.PARAMETER OutDir
    Output directory. Default: target\dist.

.PARAMETER CargoProfile
    Cargo profile: release (default) or debug.

.PARAMETER SkipBuild
    Do not run cargo (use an existing target\<profile>\materialfy.exe).

.PARAMETER SkipInstaller
    Skip the Inno Setup step.

.PARAMETER Portable
    Also pack the folder into Materialfy-<version>-windows-x64-portable.zip.
#>
param(
    [string]$QtDir,
    [string]$Version,
    [string]$OutDir,
    [ValidateSet("release", "debug")]
    [string]$CargoProfile = "release",
    [switch]$SkipBuild,
    [switch]$SkipInstaller,
    [switch]$Portable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
# UTF-8 output in any console code page (cp866/cp1251)
try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false) } catch { }

$AppName = "Materialfy"
$ExeName = "materialfy.exe"
$FilePrefix = "Materialfy"

$ProjectRoot = $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $ProjectRoot "target\dist" }
$OutDir = [IO.Path]::GetFullPath($OutDir)
$Stage = Join-Path $OutDir "materialfy"

function Write-Step { param([string]$msg) Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$msg) Write-Host "    OK: $msg" -ForegroundColor Green }
function Write-Warn { param([string]$msg) Write-Host "    WARN: $msg" -ForegroundColor Yellow }
function Abort      { param([string]$msg) Write-Host "`nERROR: $msg" -ForegroundColor Red; exit 1 }

# ─── Версия ───────────────────────────────────────────────────────────────────
if (-not $Version) {
    $cargoToml = Get-Content (Join-Path $ProjectRoot "Cargo.toml") -Raw
    if ($cargoToml -match '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
    } else {
        $Version = "0.0.0"
    }
}
$Version = $Version.TrimStart("v")
Write-Host "$AppName $Version → $OutDir" -ForegroundColor White

# ─── 1. Qt ────────────────────────────────────────────────────────────────────
Write-Step "Looking for Qt 6.12 (MSVC)"
$candidates = @()
if ($QtDir) { $candidates += $QtDir }
if ($env:QTDIR) { $candidates += $env:QTDIR }
if ($env:QMAKE) { $candidates += (Split-Path (Split-Path $env:QMAKE)) }
$candidates += @(
    "C:\Qt\6.12.0\msvc2022_64",
    (Join-Path $HOME "Qt\6.12.0\msvc2022_64"),
    (Join-Path $HOME "Qt\6.12.0")
)
$QtDir = $null
foreach ($c in $candidates) {
    if ($c -and (Test-Path (Join-Path $c "bin\qmake6.exe"))) { $QtDir = (Resolve-Path $c).Path; break }
}
if (-not $QtDir) {
    Abort ("Qt 6.12 MSVC not found. Pass -QtDir or install it:`n" +
        "    python .github\scripts\install_qt.py --out C:\Qt\6.12.0\msvc2022_64")
}
$Qmake = Join-Path $QtDir "bin\qmake6.exe"
$WinDeployQt = Join-Path $QtDir "bin\windeployqt.exe"
if (-not (Test-Path $WinDeployQt)) { $WinDeployQt = Join-Path $QtDir "bin\windeployqt6.exe" }
if (-not (Test-Path $WinDeployQt)) { Abort "windeployqt not found in $QtDir\bin" }
$env:QMAKE = $Qmake
$env:QTDIR = $QtDir
$env:PATH = "$QtDir\bin;$env:PATH"
Write-Ok "$QtDir ($(& $Qmake -query QT_VERSION))"

# ─── 2. cargo build ───────────────────────────────────────────────────────────
$Exe = Join-Path $ProjectRoot "target\$CargoProfile\$ExeName"
if (-not $SkipBuild) {
    Write-Step "cargo build ($CargoProfile) -p materialfy"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Abort "cargo not found. Install Rust: https://rustup.rs (toolchain stable-x86_64-pc-windows-msvc)"
    }
    Push-Location $ProjectRoot
    try {
        # --locked: Cargo.lock держит vergen 9.0.6 (9.1 ломает build-скрипт librespot-core)
        if ($CargoProfile -eq "release") {
            cargo build --locked --release -p materialfy
        } else {
            cargo build --locked -p materialfy
        }
        if ($LASTEXITCODE -ne 0) { Abort "cargo build failed" }
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path $Exe)) { Abort "$Exe not found" }
Write-Ok $Exe

# ─── 3. Папка дистрибутива ────────────────────────────────────────────────────
Write-Step "Assembling $Stage"
if (Test-Path $Stage) { Remove-Item $Stage -Recurse -Force }
New-Item -ItemType Directory -Force $Stage | Out-Null
Copy-Item $Exe $Stage

# QML и ресурсы — рядом с exe: оболочка ищет <exe>\qml, assets — соседний каталог
$srcQml = Join-Path $ProjectRoot "src\materialfy\qml"
$srcAssets = Join-Path $ProjectRoot "src\materialfy\assets"
Copy-Item $srcQml (Join-Path $Stage "qml") -Recurse
Get-ChildItem (Join-Path $Stage "qml") -Recurse -Directory -Filter "__pycache__" |
    Remove-Item -Recurse -Force
$assets = Join-Path $Stage "assets"
New-Item -ItemType Directory -Force $assets | Out-Null
Copy-Item (Join-Path $srcAssets "fonts") (Join-Path $assets "fonts") -Recurse   # fonts-src (исходники шрифтов) не нужен
Get-ChildItem $srcAssets -File | Where-Object { $_.Extension -in ".svg", ".png" } |
    Copy-Item -Destination $assets

$deployArgs = @("--qmldir", $srcQml, "--no-opengl-sw", "--no-compiler-runtime", "--translations", "ru,pt")   # Qt's own strings for the Russian and Portuguese UI; English needs no catalog
if ($CargoProfile -eq "release") { $deployArgs = @("--release") + $deployArgs }
& $WinDeployQt @deployArgs (Join-Path $Stage $ExeName)
if ($LASTEXITCODE -ne 0) { Abort "windeployqt failed with exit code $LASTEXITCODE" }
Write-Ok "Qt deployed by windeployqt"

# Рантайм MSVC (vcruntime140*.dll, msvcp140*.dll) кладём рядом с exe — без vc_redist у пользователя
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
$crtDir = $null
if (Test-Path $vswhere) {
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath | Select-Object -First 1
    if ($vs) {
        $crtDir = Get-ChildItem (Join-Path $vs "VC\Redist\MSVC") -Directory -ErrorAction SilentlyContinue |
            Sort-Object { try { [version]$_.Name } catch { [version]"0.0" } } -Descending |
            ForEach-Object { Get-ChildItem (Join-Path $_.FullName "x64") -Directory -Filter "Microsoft.VC*.CRT" -ErrorAction SilentlyContinue } |
            Select-Object -First 1
    }
}
if ($crtDir) {
    Copy-Item (Join-Path $crtDir.FullName "*.dll") $Stage
    Write-Ok "MSVC runtime: $($crtDir.FullName)"
} else {
    Write-Warn "MSVC runtime not found: a clean system will need the Visual C++ Redistributable"
}

$sizeMB = [math]::Round(((Get-ChildItem $Stage -Recurse -File | Measure-Object Length -Sum).Sum) / 1MB, 0)
Write-Ok "folder ready: $Stage ($sizeMB MB)"

if ($Portable) {
    $zip = Join-Path $OutDir "$FilePrefix-$Version-windows-x64-portable.zip"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $zip -CompressionLevel Optimal
    Write-Ok "portable archive: $zip"
}

if ($SkipInstaller) {
    Write-Host "`nDone: $Stage\$ExeName (Inno Setup skipped)" -ForegroundColor Green
    exit 0
}

# ─── 4. Inno Setup ────────────────────────────────────────────────────────────
Write-Step "Looking for Inno Setup"
$iscc = $null
$isccCmd = Get-Command ISCC -ErrorAction SilentlyContinue
foreach ($candidate in @(
    $(if ($isccCmd) { $isccCmd.Source }),
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
)) {
    if ($candidate -and (Test-Path $candidate)) { $iscc = $candidate; break }
}
if (-not $iscc) {
    Write-Warn "Inno Setup 6 not found (winget install JRSoftware.InnoSetup), skipping the installer"
    Write-Host "`nDone: $Stage\$ExeName" -ForegroundColor Green
    exit 0
}
Write-Ok $iscc

Write-Step "Building the installer"
# VersionInfoVersion — только числа: 0.2.0-rc1 → 0.2.0
$numVersion = ($Version -split '[-+]')[0]
& $iscc "/Qp" "/DAppVersion=$Version" "/DAppNumVersion=$numVersion" "/DDistDir=$Stage" "/O$OutDir" (Join-Path $ProjectRoot "materialfy.iss")
if ($LASTEXITCODE -ne 0) { Abort "Inno Setup failed with exit code $LASTEXITCODE" }

$setupExe = Join-Path $OutDir "$FilePrefix-$Version-Setup.exe"
if (-not (Test-Path $setupExe)) { Abort "$setupExe not found after ISCC" }
$sizeMB = [math]::Round((Get-Item $setupExe).Length / 1MB, 0)
Write-Host "`nDone:" -ForegroundColor Green
Write-Host "  Installer:  $setupExe ($sizeMB MB)" -ForegroundColor White
Write-Host "  Portable:   $Stage\$ExeName" -ForegroundColor White
