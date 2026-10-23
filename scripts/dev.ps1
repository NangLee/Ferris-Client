[CmdletBinding()]
param([string]$PublicKey = $env:FERRIS_UPDATE_PUBLIC_KEY, [switch]$Minimized)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
Assert-FerrisCommand 'cargo' 'Install Rust stable from https://rustup.rs and the Visual C++ build tools.'
$previous = $env:FERRIS_UPDATE_PUBLIC_KEY
try {
    if (-not $PublicKey) {
        $publicKeyPath = Join-Path (Get-FerrisRoot) 'public-signing-key.txt'
        if (Test-Path -LiteralPath $publicKeyPath -PathType Leaf) { $PublicKey = [IO.File]::ReadAllText($publicKeyPath).Trim() }
    }
    if ($PublicKey) { $env:FERRIS_UPDATE_PUBLIC_KEY = Assert-FerrisPublicKey $PublicKey }
    else { Write-Warning 'This development build has no pinned key. Signed runtime installation will be unavailable until rebuilt with the Ferris public key.' }
    Push-Location (Get-FerrisRoot)
    try {
        $arguments = @('run', '--locked', '--bin', 'ferris-client', '--')
        if ($Minimized) { $arguments += '--minimized' }
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) { throw "Development client exited with code $LASTEXITCODE." }
    } finally { Pop-Location }
} finally { $env:FERRIS_UPDATE_PUBLIC_KEY = $previous }
