# EverEvo — one-click asset bundler.
#
# Usage:
#   .\scripts\bundle-assets.ps1                               # Bundle for current host
#   .\scripts\bundle-assets.ps1 -Target aarch64-unknown-linux-gnu  # Bundle for ARM64 Linux
#   .\scripts\bundle-assets.ps1 -All                          # Bundle for ALL 5 platforms
#   .\scripts\bundle-assets.ps1 -Release -SkipGit -SkipRerankerCn
#
# On failure: fix the issue, then re-run the SAME command.
# Already-bundled assets are skipped — only the failed one will be retried.

param(
    [switch]$Release,
    [switch]$All,
    [string]$Target,
    [switch]$SkipGit,
    [switch]$SkipRerankerCn
)

$ErrorActionPreference = "Stop"
Push-Location (Split-Path -Parent $PSScriptRoot)

# ── Compute target list ──────────────────────────────────────────────
$targets = if ($All) {
    @("x86_64-pc-windows-msvc","aarch64-apple-darwin","x86_64-apple-darwin",
      "x86_64-unknown-linux-gnu","aarch64-unknown-linux-gnu")
} elseif ($Target) {
    @($Target)
} else {
    @((rustc -vV | Select-String "host:").ToString() -replace "host: ", "")
}

# ── Build cargo args ─────────────────────────────────────────────────
$buildFlags = if ($Release) { @("--release") } else { @() }
$skipFlags = @()
if ($SkipGit)           { $skipFlags += "--skip-git" }
if ($SkipRerankerCn)    { $skipFlags += "--skip-reranker-cn" }

$totalTargets = $targets.Count
$doneCount = 0
$failedTargets = @()

foreach ($t in $targets) {
    $doneCount++
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "[$doneCount/$totalTargets] Target: $t" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $outDir = "resources/bundled/$t"
    $cargoArgs = @("run", "--bin", "everevo-bundler") + $buildFlags `
        + @("--", "--target", $t, "--output", $outDir) + $skipFlags

    Write-Host "cargo $($cargoArgs -join ' ')" -ForegroundColor Yellow
    & cargo $cargoArgs

    if ($LASTEXITCODE -ne 0) {
        Write-Host "`n✗ FAILED: $t (exit code $LASTEXITCODE)" -ForegroundColor Red
        Write-Host "  Fix the issue and re-run the SAME command." -ForegroundColor Yellow
        Write-Host "  Already-bundled assets will be skipped on re-run." -ForegroundColor Yellow
        $failedTargets += $t
        Pop-Location
        exit $LASTEXITCODE
    }

    $files = Get-ChildItem $outDir -ErrorAction SilentlyContinue
    if ($files) {
        $totalMb = ($files | Measure-Object -Property Length -Sum).Sum / 1MB
        Write-Host "✓ $t : $($files.Count) files, $([math]::Round($totalMb)) MB total" -ForegroundColor Green
    }
}

Pop-Location
Write-Host "`n✓ All $totalTargets target(s) bundled successfully." -ForegroundColor Green
