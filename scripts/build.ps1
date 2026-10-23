[CmdletBinding()]
param([string]$PublicKey = $env:FERRIS_UPDATE_PUBLIC_KEY)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
Assert-FerrisCommand 'cargo' 'Install Rust stable from https://rustup.rs and the Visual C++ build tools.'
$key = Assert-FerrisPublicKey $PublicKey
$previousKey = $env:FERRIS_UPDATE_PUBLIC_KEY
$previousFlags = $env:RUSTFLAGS
try {
    $env:FERRIS_UPDATE_PUBLIC_KEY = $key
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT -and $env:RUSTFLAGS -notmatch '\+crt-static') {
        $env:RUSTFLAGS = (($env:RUSTFLAGS + ' -C target-feature=+crt-static').Trim())
    }
    Push-Location (Get-FerrisRoot)
    try {
        & cargo build --release --locked --bin ferris-client
        if ($LASTEXITCODE -ne 0) { throw "Client release build failed with code $LASTEXITCODE." }
        Write-Host 'Release binary: target/release/ferris-client.exe (Windows) or target/release/ferris-client (Linux).'
    } finally { Pop-Location }
} finally {
    $env:FERRIS_UPDATE_PUBLIC_KEY = $previousKey
    $env:RUSTFLAGS = $previousFlags
}
