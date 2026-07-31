<#
.SYNOPSIS
  EverEvo build & dev script

.PARAMETER Command
  dev     Start backend + frontend hot-reload (browser or Tauri)
  build   Compile backend + frontend (debug)
  release Build release binary + embedded frontend (distributable)
  clean   Purge build caches (target/, dist/) and report savings
  cache   Show disk usage of build caches

.EXAMPLE
  ./build.ps1 dev               # Browser mode
  ./build.ps1 dev -Tauri         # Desktop shell mode
  ./build.ps1 dev -NoFrontend    # Backend only
  ./build.ps1 build              # Debug build
  ./build.ps1 build -Release     # Release build
  ./build.ps1 release            # Full release
  ./build.ps1 clean              # Free disk space
  ./build.ps1 cache              # Check cache sizes
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet("dev", "build", "release", "clean", "cache")]
    [string]$Command = "dev",

    [switch]$Tauri,
    [switch]$NoFrontend,
    [switch]$Release,
    [switch]$Open
)

$ErrorActionPreference = "Continue"
$root = $PSScriptRoot
Set-Location $root

$env:RUST_LOG = if ($env:RUST_LOG) { $env:RUST_LOG } else { "everevo=debug,info" }

function Invoke-PreflightCheck {
    Write-Host "[check] cargo check --workspace --offline..." -ForegroundColor DarkGray -NoNewline
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    cargo check --workspace --offline 2>&1 | Out-Null
    $sw.Stop()
    $elapsed = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    if ($LASTEXITCODE -ne 0) {
        Write-Host " FAILED (${elapsed}s)" -ForegroundColor Red
        Write-Host "  Run without --offline first to fetch deps, then retry." -ForegroundColor Yellow
        exit 1
    }
    Write-Host " OK (${elapsed}s)" -ForegroundColor Green
}

function Start-Backend {
    Write-Host "[backend] http://127.0.0.1:3000" -ForegroundColor Green
    Start-Process cargo -ArgumentList "run -- serve" -NoNewWindow
}

function Start-Frontend {
    Write-Host "[frontend] http://localhost:5173" -ForegroundColor Green
    Start-Process cmd -ArgumentList "/c npm run dev" -WorkingDirectory "$root\frontend" -NoNewWindow
}

function Start-Tauri {
    $env:CARGO_NET_OFFLINE = "true"
    $ortBase = "$root\data\runtime\onnxruntime"
    $ortLib = if (Test-Path "$ortBase\lib\onnxruntime.lib") { "$ortBase\lib" }
              else { Get-ChildItem "$ortBase\*\lib\onnxruntime.lib" -ErrorAction SilentlyContinue | Select-Object -First 1 | Split-Path -Parent }
    if ($ortLib) { $env:ORT_LIB_PATH = $ortLib }
    $ortDll = if (Test-Path "$ortBase\lib\onnxruntime.dll") { "$ortBase\lib\onnxruntime.dll" }
              else { Get-ChildItem "$ortBase\*\lib\onnxruntime.dll" -ErrorAction SilentlyContinue | Select-Object -First 1 | ForEach-Object { $_.FullName } }
    if ($ortDll) { $env:ORT_DYLIB_PATH = $ortDll }
    Write-Host "[tauri] Desktop shell (offline)" -ForegroundColor Magenta
    & cmd /c "npx tauri dev"
    exit 0
}

function Invoke-Build {
    param([bool]$isRelease)
    $profileArg = if ($isRelease) { "--release" } else { "" }

    if (-not $NoFrontend) {
        Write-Host "[1/2] Building frontend..." -ForegroundColor Yellow
        Set-Location "$root\frontend"
        npm run build
        if ($LASTEXITCODE -ne 0) { throw "Frontend build failed" }
        Set-Location $root
    }

    Write-Host "[2/2] Building backend..." -ForegroundColor Yellow
    cargo build $profileArg
    if ($LASTEXITCODE -ne 0) { throw "Backend build failed" }

    $target = if ($isRelease) { "target\release" } else { "target\debug" }
    Write-Host "Done: $target\everevo-server.exe" -ForegroundColor Green

    if ($isRelease) {
        Write-Host ""
        Write-Host "Distributable package:" -ForegroundColor Cyan
        Write-Host "  $target\everevo-server.exe" -ForegroundColor White
        Write-Host "  frontend\dist\             (embedded frontend)" -ForegroundColor White
        Write-Host "  data\                      (runtime data, auto-created)" -ForegroundColor White
    }
}

# ── Main ────────────────────────────────────────────────────────────────

switch ($Command) {
    "dev" {
        Write-Host "=== EverEvo Dev Mode ===" -ForegroundColor Cyan
        Invoke-PreflightCheck
        taskkill /f /im everevo-server.exe 2>$null *>$null
        Start-Sleep -Milliseconds 300

        if ($Tauri) {
            Start-Tauri
        }

        Start-Backend
        if (-not $NoFrontend) { Start-Frontend }

        Write-Host "Log: $env:RUST_LOG" -ForegroundColor Gray
        Write-Host "Stop: taskkill /f /im everevo-server.exe" -ForegroundColor Gray
        try { while ($true) { Start-Sleep -Seconds 1 } }
        finally { taskkill /f /im everevo-server.exe 2>$null *>$null }
    }

    "build" {
        Write-Host "=== EverEvo Build ===" -ForegroundColor Cyan
        Invoke-PreflightCheck
        taskkill /f /im everevo-server.exe 2>$null *>$null
        Invoke-Build -isRelease $Release

        if ($Open) {
            $dir = if ($Release) { "release" } else { "debug" }
            Start-Process "$root\target\$dir\everevo-server.exe" -ArgumentList "serve"
        }
    }

    "release" {
        Write-Host "=== EverEvo Release Build ===" -ForegroundColor Cyan
        Invoke-PreflightCheck
        taskkill /f /im everevo-server.exe 2>$null *>$null
        $NoFrontend = $false
        Invoke-Build -isRelease $true
    }

    "clean" {
        Write-Host "=== Cache Cleanup ===" -ForegroundColor Cyan
        $dirs = @(
            "$root\target",
            "$root\src-tauri\target",
            "$root\frontend\dist",
            "$root\frontend\node_modules\.vite"
        )
        $total = 0
        foreach ($d in $dirs) {
            if (Test-Path $d) {
                $size = (Get-ChildItem $d -Recurse -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
                $total += $size
                Remove-Item -Recurse -Force $d -ErrorAction SilentlyContinue
                Write-Host ("  Removed {0} ({1:N1} MB)" -f $d, ($size/1MB)) -ForegroundColor Green
            }
        }
        Write-Host ("Total freed: {0:N1} MB" -f ($total/1MB)) -ForegroundColor Cyan
    }

    "cache" {
        Write-Host "=== Cache Sizes ===" -ForegroundColor Cyan
        $dirs = @(
            @{n="target (workspace)"; p="$root\target"},
            @{n="target (tauri)";     p="$root\src-tauri\target"},
            @{n="frontend dist";      p="$root\frontend\dist"},
            @{n="frontend node_modules"; p="$root\frontend\node_modules"},
            @{n="data runtime";       p="$root\data\runtime"},
            @{n="data models";        p="$root\data\models"},
            @{n="data downloads";     p="$root\data\downloads"},
            @{n="data db";            p="$root\data\db"}
        )
        foreach ($d in $dirs) {
            if (Test-Path $d.p) {
                $size = (Get-ChildItem $d.p -Recurse -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
                Write-Host ("  {0,-25} {1,8:N1} MB" -f $d.n, ($size/1MB))
            } else {
                Write-Host ("  {0,-25}       --" -f $d.n)
            }
        }
    }
}
