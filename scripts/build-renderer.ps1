[CmdletBinding()]
param([string]$OutputPath = '')
$ErrorActionPreference = 'Stop'
Write-Warning 'build-renderer.ps1 is retired. Use scripts/build-worker.ps1. No Juz source is built or packaged.'
& (Join-Path $PSScriptRoot 'build-worker.ps1') -OutputPath $OutputPath
