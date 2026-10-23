[CmdletBinding()]
param([string]$PublicKey = $env:FERRIS_UPDATE_PUBLIC_KEY, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Use package.sh to package on Linux.' }
$null = Assert-FerrisPublicKey $PublicKey
$root = Get-FerrisRoot
if (-not $SkipBuild) { & (Join-Path $PSScriptRoot 'build.ps1') -PublicKey $PublicKey }
$binary = Join-Path $root 'target\release\ferris-client.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'The release executable is missing. Run build.ps1 first.' }
$compiler = Get-Command 'makensis.exe' -ErrorAction SilentlyContinue
if (-not $compiler) {
    foreach ($directory in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $directory) { continue }
        $candidate = Join-Path $directory 'NSIS\makensis.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $compiler = Get-Item -LiteralPath $candidate; break }
    }
}
if (-not $compiler) { throw 'Install NSIS 3 from https://nsis.sourceforge.io, or run: winget install NSIS.NSIS. Then rerun package.ps1.' }
$compilerPath = if ($compiler -is [IO.FileInfo]) { $compiler.FullName } else { $compiler.Source }
$dist = Join-Path $root 'dist'
$null = New-Item -ItemType Directory -Path $dist -Force
$version = Get-FerrisVersion $root
$output = Join-Path $dist "ferris-client-$version-windows-x86_64-setup.exe"
$arguments = @('/V3', "/DFERRIS_VERSION=$version", "/DFERRIS_BINARY=$binary", "/DFERRIS_NOTICES=$(Join-Path $root 'THIRD_PARTY.md')", "/DFERRIS_LICENSE=$(Join-Path $root 'LICENSE.md')", "/DFERRIS_OUTFILE=$output", (Join-Path $PSScriptRoot 'windows-installer.nsi'))
& $compilerPath @arguments
if ($LASTEXITCODE -ne 0) { throw "NSIS packaging failed with code $LASTEXITCODE." }
Write-FerrisArtifact $output 'none' ([IO.Path]::GetFileName($output))
