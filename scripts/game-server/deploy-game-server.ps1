# One command from your PC to a fresh game VPS: builds pack-sync for Linux with your settings,
# copies the kit and installs it. The VPS must accept `ssh <alias>` as root.
#   .\scripts\game-server\deploy-game-server.ps1 -SshHost gamevps -PackDir <pack folder> `
#       -Player YourNick -AcceptEula
# -AcceptEula: you have read https://aka.ms/MinecraftEULA and accept it for this server.
# On a VPS shared with other services add limits, for example -MemoryMax 1400M -CpuQuota 120%.
param(
    [Parameter(Mandatory)][string]$SshHost,
    [Parameter(Mandatory)][string]$PackDir,
    [Parameter(Mandatory)][string]$Player,
    [Parameter(Mandatory)][string]$ServerKeyFile,
    [switch]$AcceptEula,
    [ValidatePattern('^(|\d+[KMG])$')][string]$MemoryMax = '',
    [ValidatePattern('^(|\d+%)$')][string]$CpuQuota = ''
)
$ErrorActionPreference = 'Stop'

$GitBash = 'C:\Program Files\Git\bin\bash.exe'
$Scripts = Split-Path -Parent $PSScriptRoot
$Binary = Join-Path (Split-Path -Parent $Scripts) '.build\linux\pack-sync'
$Config = Join-Path $PackDir 'pack-sync.toml'
$Remote = '/tmp/pack-sync-kit'

function Assert-Success([string]$What) {
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

foreach ($file in $Config, (Join-Path $PackDir 'pack.json'), $ServerKeyFile) {
    if (-not (Test-Path -LiteralPath $file)) { throw "$file is missing." }
}
if ((Get-Content -Raw -LiteralPath $Config) -notmatch '(?m)^\s*site\s*=\s*"(https://[^"]+)"') {
    throw "$Config has no https site."
}
$Site = $Matches[1]

# The server runs the same Nightly the mods are built on: nightly-2026-11-08-fdc87ae is
# published as nightly-2026-11-08 once a newer Nightly replaces it.
$Base = (Get-Content -Raw -LiteralPath (Join-Path $PackDir 'pack.json') | ConvertFrom-Json).horizon_base
$Release = if ($Base -match '^(nightly-\d{4}-\d{2}-\d{2})-[0-9a-f]+$') { $Matches[1] } else { $Base }
gh release view $Release --repo horizon-rust/horizon --json tagName 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host "$Release is not published under its own tag yet; using the current Nightly."
    $Release = 'nightly'
}

& $GitBash (Join-Path $Scripts 'build-linux.sh') ((Resolve-Path -LiteralPath $Config).Path -replace '\', '/')
Assert-Success 'Building pack-sync for Linux'

ssh $SshHost "rm -rf $Remote && mkdir -p $Remote"
Assert-Success 'Preparing the VPS'
$kit = 'install-game-server.sh', 'uninstall-game-server.sh', 'horizon-server.service', 'pack-sync-server.service' |
    ForEach-Object { Join-Path $PSScriptRoot $_ }
scp $Binary @kit "${SshHost}:$Remote/"
Assert-Success 'Copying the kit'

$options = " --site $Site --player $Player --horizon-release $Release"
if ($AcceptEula) { $options += ' --accept-eula' }
if ($MemoryMax) { $options += " --memory-max $MemoryMax" }
if ($CpuQuota) { $options += " --cpu-quota $CpuQuota" }
Get-Content -Raw -LiteralPath $ServerKeyFile | ssh $SshHost "bash $Remote/install-game-server.sh$options"
Assert-Success 'Installing on the VPS'
Write-Host 'Done. Watch it pick up the newest build: ssh' $SshHost "'journalctl -u pack-sync-server -f'"
