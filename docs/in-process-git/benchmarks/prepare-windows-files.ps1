# SPDX-License-Identifier: GPL-3.0-only

<#
.SYNOPSIS
파일 구성의 메모리 비교에 재사용할 임시 저장소 네 개를 만들거나 관측 후 정리한다.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Manifest,
    [switch]$Cleanup
)

$ErrorActionPreference = 'Stop'
$manifestPath = [System.IO.Path]::GetFullPath($Manifest)
$ownerPath = "$manifestPath.owner.json"
$temporaryParent = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\')
$utf8 = [System.Text.UTF8Encoding]::new($false)

<#
.SYNOPSIS
소유 표식과 절대 경로를 대조한 전용 임시 저장소만 정리하며 불일치하면 실패한다.
#>
function Remove-PreparedFixture {
    param([string]$Root, [string]$Nonce)
    if (-not (Test-Path -LiteralPath $Root)) { return }
    $resolved = (Resolve-Path -LiteralPath $Root).Path
    $marker = Join-Path $resolved 'fixture-owner.json'
    if ([System.IO.Path]::GetDirectoryName($resolved) -ne $temporaryParent -or
        [System.IO.Path]::GetFileName($resolved) -ne "gct-bench-$Nonce" -or
        $Nonce -notmatch '^[0-9a-f]{32}$' -or
        (Get-Item -LiteralPath $resolved).Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) -or
        -not (Test-Path -LiteralPath $marker) -or
        (Get-Content -Raw -LiteralPath $marker | ConvertFrom-Json).nonce -ne $Nonce) {
        throw '준비한 fixture의 경로 또는 소유 표식이 일치하지 않습니다.'
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

if ($Cleanup) {
    $owner = Get-Content -Raw -LiteralPath $ownerPath | ConvertFrom-Json
    Remove-PreparedFixture -Root $owner.root -Nonce $owner.nonce
    Remove-Item -LiteralPath $manifestPath,$ownerPath
    Write-Output 'Prepared fixture removed'
    exit 0
}
if ((Test-Path -LiteralPath $manifestPath) -or (Test-Path -LiteralPath $ownerPath)) {
    throw '기존 manifest를 덮어쓰지 않습니다. 먼저 해당 fixture를 정리하세요.'
}
$nonce = [guid]::NewGuid().ToString('N')
$fixtureRoot = Join-Path $temporaryParent "gct-bench-$nonce"
New-Item -ItemType Directory -Path $fixtureRoot -ErrorAction Stop | Out-Null
$owner = [pscustomobject]@{root=$fixtureRoot;nonce=$nonce}
[System.IO.File]::WriteAllText((Join-Path $fixtureRoot 'fixture-owner.json'),($owner | ConvertTo-Json),$utf8)
$gitExecutable = (Get-Command git -CommandType Application | Select-Object -First 1).Source
$config = New-TemporaryFile
$environmentNames = @('GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_AUTHOR_NAME','GIT_AUTHOR_EMAIL','GIT_COMMITTER_NAME','GIT_COMMITTER_EMAIL','GIT_TERMINAL_PROMPT','LC_ALL')
$previousEnvironment = @{}
foreach ($name in $environmentNames) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$env:GIT_CONFIG_GLOBAL = $config.FullName
$env:GIT_CONFIG_NOSYSTEM = '1'
$env:GIT_AUTHOR_NAME = 'fixture'
$env:GIT_COMMITTER_NAME = 'fixture'
$env:GIT_AUTHOR_EMAIL = 'fixture@example.com'
$env:GIT_COMMITTER_EMAIL = 'fixture@example.com'
$env:GIT_TERMINAL_PROMPT = '0'
$env:LC_ALL = 'C'
$prepared = $false

<#
.SYNOPSIS
생성용 Git 명령을 소유한 디렉터리 안에서만 실행하고 실패하면 중단한다.
#>
function Invoke-FixtureGit {
    param([string]$Repository, [string[]]$GitArguments)
    $absolute = [System.IO.Path]::GetFullPath($Repository)
    if ($absolute -ne $fixtureRoot -and -not $absolute.StartsWith($fixtureRoot + '\',[System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'fixture 밖에서는 Git 구성을 변경하지 않습니다.'
    }
    # 테스트 데이터 생성 명령이다. 제품의 GitRunner·guard를 대체하지 않는다.
    & $gitExecutable -C $absolute -c commit.gpgsign=false -c core.hooksPath= @GitArguments
    if ($LASTEXITCODE -ne 0) { throw "fixture Git 구성 실패: $($GitArguments[0])" }
}

<#
.SYNOPSIS
같은 내용의 추적 파일 10000개와 지정한 이력 파일을 PowerShell에서 준비한다.
#>
function Write-FixtureFiles {
    param([string]$Repository, [string]$History)
    $files = Join-Path $Repository '많은 파일'
    New-Item -ItemType Directory -Path $files -ErrorAction Stop | Out-Null
    for ($index = 0; $index -lt 10000; $index++) {
        [System.IO.File]::WriteAllText((Join-Path $files ('file-{0:D5}.txt' -f $index)),"unchanged`n",$utf8)
    }
    [System.IO.File]::WriteAllText((Join-Path $Repository 'history.txt'),$History,$utf8)
}

try {
    $paths = [System.Collections.Generic.List[string]]::new()
    for ($index = 0; $index -lt 4; $index++) {
        $seed = Join-Path $fixtureRoot "seed $index"
        $origin = Join-Path $fixtureRoot "origin $index.git"
        $repository = Join-Path $fixtureRoot "측정 저장소 $index"
        $worktree = Join-Path $fixtureRoot "파일 검사 worktree $index"
        New-Item -ItemType Directory -Path $seed -ErrorAction Stop | Out-Null
        Invoke-FixtureGit -Repository $seed -GitArguments @('init','-q','-b','trunk')
        [System.IO.File]::WriteAllText((Join-Path $seed 'history.txt'),"initial`n",$utf8)
        Invoke-FixtureGit -Repository $seed -GitArguments @('add','--','history.txt')
        Invoke-FixtureGit -Repository $seed -GitArguments @('commit','-q','-m','initial')
        Invoke-FixtureGit -Repository $fixtureRoot -GitArguments @('clone','-q','--bare','--',$seed,$origin)
        Invoke-FixtureGit -Repository $fixtureRoot -GitArguments @('clone','-q','--',$origin,$repository)
        Write-FixtureFiles -Repository $repository -History "initial`ntracked files`n"
        Invoke-FixtureGit -Repository $repository -GitArguments @('add','--','.')
        Invoke-FixtureGit -Repository $repository -GitArguments @('commit','-q','-m','tracked files')
        Invoke-FixtureGit -Repository $repository -GitArguments @('branch','--','완료')
        [System.IO.File]::WriteAllText((Join-Path $repository 'history.txt'),"initial`ntracked files`nbase advances`n",$utf8)
        Invoke-FixtureGit -Repository $repository -GitArguments @('add','--','history.txt')
        Invoke-FixtureGit -Repository $repository -GitArguments @('commit','-q','-m','base advances')
        Invoke-FixtureGit -Repository $repository -GitArguments @('worktree','add','-q','--no-checkout','--',$worktree,'완료')
        Write-FixtureFiles -Repository $worktree -History "initial`ntracked files`n"
        # Git으로 인덱스를 만든다. .git 내부 파일을 직접 작성하지 않는다.
        Invoke-FixtureGit -Repository $worktree -GitArguments @('reset','--mixed','-q','HEAD','--')
        foreach ($path in @($repository,$worktree)) {
            $status = @(Invoke-FixtureGit -Repository $path -GitArguments @('status','--porcelain','--untracked-files=all'))
            $tracked = @(Invoke-FixtureGit -Repository $path -GitArguments @('ls-files'))
            if ($status.Count -ne 0 -or $tracked.Count -ne 10001) { throw '준비한 fixture의 파일 상태가 예상과 다릅니다.' }
        }
        $commits = Invoke-FixtureGit -Repository $repository -GitArguments @('rev-list','--count','--all')
        if ([int]$commits -ne 3) { throw 'fixture 커밋 수가 예상과 다릅니다.' }
        $paths.Add($repository)
        Write-Output "Prepared repository $($index + 1)/4"
    }
    [System.IO.File]::WriteAllText($manifestPath,($paths.ToArray() | ConvertTo-Json),$utf8)
    [System.IO.File]::WriteAllText($ownerPath,($owner | ConvertTo-Json),$utf8)
    $prepared = $true
    Write-Output 'Prepared four clean repositories with one linked worktree and 10000 data files each'
} finally {
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name,$previousEnvironment[$name],'Process') }
    Remove-Item -LiteralPath $config.FullName
    if (-not $prepared) {
        Remove-PreparedFixture -Root $fixtureRoot -Nonce $nonce
    }
}
