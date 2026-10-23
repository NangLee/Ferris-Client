[CmdletBinding()]
param([string]$OutputPath = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Use build-worker.sh on Linux.' }
Assert-FerrisCommand 'cargo' 'Install Rust stable and the Visual C++ build tools.'
Assert-FerrisCommand 'rustc' 'Install Rust stable.'
$hostTriple = (& rustc -vV | Select-String '^host: ').ToString().Substring(6).Trim()
if ($hostTriple -ne 'x86_64-pc-windows-msvc') { throw 'This script packages a native Windows x86-64 MSVC worker.' }
$root = Get-FerrisRoot
$manifest = Join-Path $root 'crates\ferris-graphics\Cargo.toml'
$target = Join-Path $root 'target\graphics-worker'
$previousFlags = $env:RUSTFLAGS
try {
    if ($env:RUSTFLAGS -notmatch '\+crt-static') { $env:RUSTFLAGS = (($env:RUSTFLAGS + ' -C target-feature=+crt-static').Trim()) }
    & cargo build --manifest-path $manifest --release --locked --bin ferris-worker --target-dir $target
    if ($LASTEXITCODE -ne 0) { throw "Ferris worker build failed with code $LASTEXITCODE." }
} finally { $env:RUSTFLAGS = $previousFlags }
$dist = Join-Path $root 'dist'
$null = New-Item -ItemType Directory -Path $dist -Force
$version = Get-FerrisVersion $root
if (-not $OutputPath) { $OutputPath = Join-Path $dist "ferris-worker-$version-windows-x86_64.zip" }
$OutputPath = [IO.Path]::GetFullPath($OutputPath)
if ([IO.Path]::GetExtension($OutputPath) -ne '.zip') { throw 'Windows worker output must end with .zip.' }
if (Test-Path -LiteralPath $OutputPath) { throw "Output already exists: $OutputPath. Choose another -OutputPath." }
$stage = Join-Path $dist ('.worker-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $stage
try {
    Copy-Item -LiteralPath (Join-Path $target 'release\ferris-worker.exe') -Destination (Join-Path $stage 'ferris-worker.exe')
    foreach ($name in @('LICENSE.md', 'THIRD_PARTY.md')) {
        Copy-Item -LiteralPath (Join-Path $root $name) -Destination (Join-Path $stage $name)
    }
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $null = New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($OutputPath)) -Force
    $archive = [IO.Compression.ZipFile]::Open($OutputPath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in @('ferris-worker.exe', 'LICENSE.md', 'THIRD_PARTY.md')) {
            $null = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $stage $name), $name, [IO.Compression.CompressionLevel]::Optimal)
        }
    } finally { $archive.Dispose() }
    Write-FerrisArtifact $OutputPath 'zip' 'ferris-worker.exe'
} finally {
    $resolvedStage = [IO.Path]::GetFullPath($stage)
    $allowedRoot = [IO.Path]::GetFullPath($dist).TrimEnd([char[]]@(92, 47)) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolvedStage.StartsWith($allowedRoot, [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($resolvedStage) -notlike '.worker-*') {
        throw 'Refusing to clean an unexpected worker staging path.'
    }
    if (Test-Path -LiteralPath $resolvedStage) { Remove-Item -LiteralPath $resolvedStage -Recurse -Force }
}
