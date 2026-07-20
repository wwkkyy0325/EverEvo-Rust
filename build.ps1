<#
.SYNOPSIS
  EverEvo build & dev script — one command for everything.

.PARAMETER Command
  dev     Start backend + frontend hot-reload (browser or Tauri shell)
  build   Compile backend + frontend (debug)
  release Build release binary + embedded frontend (distributable)

.EXAMPLE
  ./build.ps1 dev               # Browser mode
  ./build.ps1 dev -Tauri         # Desktop shell mode
  ./build.ps1 dev -NoFrontend    # Backend only
  ./build.ps1 build              # Debug build
  ./build.ps1 build -Release     # Release build
  ./build.ps1 release            # Full release: frontend + backend release
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet("dev", "build", "release")]
    [string]$Command = "dev",

    [switch]$Tauri,
    [switch]$NoFrontend,
    [switch]$Release,
    [switch]$Open           # Auto-open browser after build
)

$ErrorActionPreference = "Continue"
$root = $PSScriptRoot
Set-Location $root

$env:RUST_LOG = if ($env:RUST_LOG) { $env:RUST_LOG } else { "everevo=debug,info" }

# ── Pre-flight: cargo check --workspace --offline ─────────────────────

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
    # Force offline mode so Tauri's internal cargo invocations don't hit
    # the unreachable mirror. Cache must be warm (run once with network first).
    $env:CARGO_NET_OFFLINE = "true"
    # Find ONNX Runtime lib — flat or versioned subdirectory
    $ortBase = "$root\data\runtime\onnxruntime"
    $ortLib = if (Test-Path "$ortBase\lib\onnxruntime.lib") { "$ortBase\lib" }
              else { Get-ChildItem "$ortBase\*\lib\onnxruntime.lib" -ErrorAction SilentlyContinue | Select-Object -First 1 | Split-Path -Parent }
    if ($ortLib) { $env:ORT_LIB_PATH = $ortLib }

    # Set ORT_DYLIB_PATH so the ort crate loads the correct onnxruntime.dll at runtime
    # instead of falling back to C:\Windows\System32\onnxruntime.dll (Windows ML, v1.17.1).
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
}
