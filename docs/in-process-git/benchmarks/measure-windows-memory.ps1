# SPDX-License-Identifier: GPL-3.0-only

<#
.SYNOPSIS
격리된 새 벤치마크 프로세스에서 작업 후 Windows 메모리와 대기 CPU를 기록한다.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('worktrees', 'files')]
    [string]$Case,
    [Parameter(Mandatory)]
    [ValidateSet('commands', 'gix')]
    [string]$Backend,
    [ValidateRange(1, 10000)]
    [int]$Iterations = 30,
    [Parameter(Mandatory)]
    [string]$OutputPrefix,
    [string]$Executable,
    [string]$FixtureManifest
)

$ErrorActionPreference = 'Stop'
$samplePrefix = [System.IO.Path]::GetFullPath($OutputPrefix)
$sampleCsv = "$samplePrefix.csv"
$sampleStdout = "$samplePrefix.stdout.txt"
$sampleStderr = "$samplePrefix.stderr.txt"
if ($Case -eq 'files') {
    if ([string]::IsNullOrEmpty($FixtureManifest)) {
        throw 'files 메모리 측정은 prepare-windows-files.ps1로 준비한 manifest가 필요합니다.'
    }
    $FixtureManifest = (Resolve-Path -LiteralPath $FixtureManifest).Path
} elseif (-not [string]::IsNullOrEmpty($FixtureManifest)) {
    throw '준비된 manifest는 files 구성에서만 사용합니다.'
}
if ([string]::IsNullOrEmpty($Executable)) {
    $exampleName = if ($Case -eq 'files') { 'scan_memory_benchmark' } else { 'scan_benchmark' }
    $Executable = Join-Path $PSScriptRoot "../../../target/release/examples/$exampleName.exe"
}
$benchmarkExe = (Resolve-Path -LiteralPath $Executable).Path
$benchmarkConfig = New-TemporaryFile
$previousGlobal = $env:GIT_CONFIG_GLOBAL
$previousNoSystem = $env:GIT_CONFIG_NOSYSTEM
$benchmarkProcess = $null
$env:GIT_CONFIG_GLOBAL = $benchmarkConfig.FullName
$env:GIT_CONFIG_NOSYSTEM = '1'

try {
    # files 입력을 프로세스 밖에서 준비해 대량 파일 생성·삭제가 관측에 섞이지 않게 한다.
    $benchmarkArguments = @(
        '--case', $Case, '--backend', $Backend, '--parallelism', '4',
        '--warmups', '5', '--iterations', $Iterations, '--hold-seconds', '30',
        '--output', ('"{0}"' -f $sampleCsv)
    )
    if ($Case -eq 'files') {
        $benchmarkArguments += @('--fixture-manifest', ('"{0}"' -f $FixtureManifest))
    }
    $benchmarkProcess = Start-Process -FilePath $benchmarkExe -ArgumentList $benchmarkArguments -WindowStyle Hidden -PassThru -RedirectStandardOutput $sampleStdout -RedirectStandardError $sampleStderr
    $ready = $false
    while (-not $benchmarkProcess.HasExited) {
        if (Test-Path -LiteralPath $sampleStdout) {
            $ready = [bool](Select-String -LiteralPath $sampleStdout -Pattern '^memory_sample_pid=' -Quiet)
            if ($ready) { break }
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) {
        throw "메모리 측정 신호 전에 종료했습니다. $sampleStderr 파일을 확인하세요."
    }

    $afterReady = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds 5
    $benchmarkProcess.Refresh()
    $privateBytes = $benchmarkProcess.PrivateMemorySize64
    $workingSet = $benchmarkProcess.WorkingSet64
    $peakWorkingSet = $benchmarkProcess.PeakWorkingSet64
    $threadCount = $benchmarkProcess.Threads.Count
    $observedDelay = $afterReady.Elapsed.TotalMilliseconds
    $cpuStart = $benchmarkProcess.TotalProcessorTime.TotalMilliseconds
    $cpuWindow = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds 5
    $benchmarkProcess.Refresh()
    $cpuMs = $benchmarkProcess.TotalProcessorTime.TotalMilliseconds - $cpuStart
    $cpuWindowMs = $cpuWindow.Elapsed.TotalMilliseconds
    if ($benchmarkProcess.HasExited) { throw '메모리 관측 중 예제 프로세스가 먼저 종료됐습니다.' }
    $observation = [pscustomobject]@{
        case = $Case
        backend = $Backend
        parallelism = 4
        iterations = $Iterations
        private_bytes = $privateBytes
        working_set_bytes = $workingSet
        # 준비 실행과 계획 비교까지 포함한 프로세스 전체의 최고값이다.
        peak_working_set_bytes = $peakWorkingSet
        threads_after_work = $threadCount
        observed_delay_ms = $observedDelay
        idle_cpu_ms = $cpuMs
        idle_window_ms = $cpuWindowMs
        idle_cpu_one_core_percent = 100 * $cpuMs / $cpuWindowMs
        fixture_cleanup = $(if ($Case -eq 'files') { 'shared-fixture' } else { 'rust' })
        cleanup_ms = 0
        after_scan_ms = $null
        cleanup_before_sample = $null
        ready_signal_age_ms = $null
    }
    $benchmarkProcess.WaitForExit()
    if ($benchmarkProcess.ExitCode -ne 0) {
        throw "벤치마크가 실패했습니다. $sampleStderr 파일을 확인하세요."
    }
    # 정상 종료까지 확인한 표본만 남긴다. 입력 fixture의 정리는 호출자가 담당한다.
    $observation | Export-Csv -LiteralPath "$samplePrefix.memory.csv" -NoTypeInformation -Encoding utf8
    Get-Content -LiteralPath "$samplePrefix.memory.csv"
} finally {
    if ($null -ne $benchmarkProcess -and -not $benchmarkProcess.HasExited) {
        $benchmarkProcess.Kill($true)
        $benchmarkProcess.WaitForExit()
    }
    if ($null -ne $benchmarkProcess) { $benchmarkProcess.Dispose() }
    $env:GIT_CONFIG_GLOBAL = $previousGlobal
    $env:GIT_CONFIG_NOSYSTEM = $previousNoSystem
    Remove-Item -LiteralPath $benchmarkConfig.FullName
}
