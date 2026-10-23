Set-StrictMode -Version Latest

function Get-FerrisRoot {
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
}

function Assert-FerrisPublicKey([string]$PublicKey) {
    if ([string]::IsNullOrWhiteSpace($PublicKey)) {
        $publicKeyPath = Join-Path (Get-FerrisRoot) 'public-signing-key.txt'
        if (Test-Path -LiteralPath $publicKeyPath -PathType Leaf) {
            $PublicKey = [IO.File]::ReadAllText($publicKeyPath).Trim()
        }
    }
    if ([string]::IsNullOrWhiteSpace($PublicKey)) {
        throw 'Set FERRIS_UPDATE_PUBLIC_KEY, pass -PublicKey, or save the public key as public-signing-key.txt. Never use the signing secret.'
    }
    $normalized = $PublicKey.Trim().Replace('-', '+').Replace('_', '/')
    $normalized = $normalized.PadRight($normalized.Length + ((4 - $normalized.Length % 4) % 4), '=')
    try { $bytes = [Convert]::FromBase64String($normalized) }
    catch { throw 'The public signing key must be base64-encoded Ed25519 key material.' }
    if ($bytes.Length -ne 32) { throw 'The public signing key must contain exactly 32 bytes.' }
    return $PublicKey.Trim()
}

function Assert-FerrisCommand([string]$Name, [string]$Instructions) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) { throw "Missing $Name. $Instructions" }
}

function Get-FerrisVersion([string]$Root) {
    $text = [IO.File]::ReadAllText((Join-Path $Root 'Cargo.toml'))
    $match = [regex]::Match($text, '(?m)^version\s*=\s*"([0-9A-Za-z.+_-]+)"')
    if (-not $match.Success) { throw 'Could not read the Cargo package version.' }
    return $match.Groups[1].Value
}

function Write-FerrisArtifact([string]$Path, [string]$Archive, [string]$Executable) {
    $item = Get-Item -LiteralPath $Path
    $metadata = [ordered]@{
        sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        bytes = $item.Length
        archive = $Archive
        executable = $Executable
    }
    $json = $metadata | ConvertTo-Json
    [IO.File]::WriteAllText($item.FullName + '.artifact.json', $json + [Environment]::NewLine, (New-Object Text.UTF8Encoding($false)))
    Write-Host $item.FullName
    Write-Host $json
}
