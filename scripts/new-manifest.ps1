param(
    [Parameter(Mandatory)][ValidateSet('windows-x86_64','linux-x86_64','linux-aarch64')][string]$Platform,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$RendererFile,
    [Parameter(Mandatory)][string]$RendererUrl,
    [string]$RendererExecutable,
    [Parameter(Mandatory)][string]$FfmpegFile,
    [Parameter(Mandatory)][string]$FfmpegUrl,
    [Parameter(Mandatory)][string]$FfmpegExecutable,
    [Parameter(Mandatory)][string]$ClientFile,
    [Parameter(Mandatory)][string]$ClientUrl,
    [string]$OutputPath
)
$ErrorActionPreference = 'Stop'
$expectedWorker = if ($Platform -eq 'windows-x86_64') { 'ferris-worker.exe' } else { 'ferris-worker' }
if (-not $RendererExecutable) { $RendererExecutable = $expectedWorker }
if ($RendererExecutable -ne $expectedWorker) { throw "The renderer artifact must contain the generic $expectedWorker at its archive root." }
function New-Artifact([string]$File, [string]$Url, [string]$Executable, [switch]$Installer) {
    $item = Get-Item -LiteralPath $File
    $uri = [uri]$Url
    if ($uri.Scheme -ne 'https' -or $uri.UserInfo) { throw 'Artifact URLs must use HTTPS without credentials.' }
    $archive = if ($Installer) { 'none' } elseif ($item.Name.EndsWith('.tar.xz')) { 'tar.xz' } elseif ($item.Extension -eq '.zip') { 'zip' } else { 'none' }
    return [ordered]@{url=$uri.AbsoluteUri;sha256=(Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant();bytes=$item.Length;archive=$archive;executable=$Executable}
}
$manifest = [ordered]@{
    runtime_kind='ferris-scene-worker-v1';scene_format='ferris-scene-v1'
    release_id=$Version;version=$Version;platform=$Platform
    artifacts=[ordered]@{
        renderer=New-Artifact $RendererFile $RendererUrl $RendererExecutable
        ffmpeg=New-Artifact $FfmpegFile $FfmpegUrl $FfmpegExecutable
        client=New-Artifact $ClientFile $ClientUrl (Split-Path -Leaf $ClientFile) -Installer
    }
}
$json = $manifest | ConvertTo-Json -Depth 6
if ($OutputPath) {
    $manifestOutputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
    $manifestOutputDirectory = [IO.Path]::GetDirectoryName($manifestOutputPath)
    $null = [IO.Directory]::CreateDirectory($manifestOutputDirectory)
    [IO.File]::WriteAllText($manifestOutputPath, $json, [Text.UTF8Encoding]::new($false))
}
$json
