# One command from your PC to a Linux server: builds pack-sync-hub, copies it and installs it as
# a service. The GitHub token is typed hidden; the game server's key is created on first use.
#   .\scripts\hub\deploy-hub.ps1 -SshHost myvps -PackDir <pack folder> -ServerKeyFile C:\keys\pack-server.key
# The server must accept `ssh <alias>` as root. Put HTTPS in front of it (Caddyfile.example).
param(
    [Parameter(Mandatory)][string]$SshHost,
    [Parameter(Mandatory)][string]$PackDir,
    [Parameter(Mandatory)][string]$ServerKeyFile
)
$ErrorActionPreference = 'Stop'

$GitBash = 'C:\Program Files\Git\bin\bash.exe'
$Scripts = Split-Path -Parent $PSScriptRoot
$Binary = Join-Path (Split-Path -Parent $Scripts) '.build\linux\pack-sync-hub'
$Config = Join-Path $PackDir 'pack-sync.toml'
$Remote = '/tmp/pack-sync-hub-kit'
$KeyCharacters = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
$KeyLength = 48

function Assert-Success([string]$What) {
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

function New-ServerKey {
    # Bytes past the largest multiple of the alphabet are dropped, so every character is equally likely.
    $limit = 256 - (256 % $KeyCharacters.Length)
    $key = [Text.StringBuilder]::new()
    while ($key.Length -lt $KeyLength) {
        foreach ($byte in [Security.Cryptography.RandomNumberGenerator]::GetBytes($KeyLength)) {
            if ($byte -lt $limit -and $key.Length -lt $KeyLength) {
                [void]$key.Append($KeyCharacters[$byte % $KeyCharacters.Length])
            }
        }
    }
    return $key.ToString()
}

$Repository = (Get-Content -Raw -LiteralPath (Join-Path $PackDir 'pack.json') | ConvertFrom-Json).repository
if (-not (Test-Path -LiteralPath $ServerKeyFile)) {
    [IO.File]::WriteAllText($ServerKeyFile, (New-ServerKey))
    Write-Host "Created the game server's key in $ServerKeyFile; keep it private."
}
$serverKey = (Get-Content -Raw -LiteralPath $ServerKeyFile).Trim()
$secure = Read-Host "GitHub token that can read $Repository (hidden)" -AsSecureString
$token = [Net.NetworkCredential]::new('', $secure).Password.Trim()
if ($token -notmatch '^github_pat_[A-Za-z0-9_]+$') {
    throw 'That is not a fine-grained GitHub token (they start with github_pat_).'
}

& $GitBash (Join-Path $Scripts 'build-linux.sh') ((Resolve-Path -LiteralPath $Config).Path -replace '\', '/')
Assert-Success 'Building for Linux'
ssh $SshHost "rm -rf $Remote && mkdir -p $Remote"
Assert-Success 'Preparing the server'
scp $Binary (Join-Path $PSScriptRoot 'install-hub.sh') (Join-Path $PSScriptRoot 'pack-sync-hub.service') "${SshHost}:$Remote/"
Assert-Success 'Copying the kit'
"$token`n$serverKey`n" | ssh $SshHost "bash $Remote/install-hub.sh --repo $Repository"
Assert-Success 'Installing the hub'
