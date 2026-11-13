# Builds the mod pack for players and the server, signs it and publishes the next release.
#   .\scripts\publish.ps1 -PackDir <your pack folder> -KeyFile <private key> -TrustNativeCode
# The pack folder holds pack.json (the mods) and pack-sync.toml (the settings baked into
# pack-sync). -NoUpload stops after signing. See docs/owner-guide.md.
param(
    [Parameter(Mandatory)][string]$PackDir,
    [Parameter(Mandatory)][string]$KeyFile,
    # The mods are compiled and run with full rights. Only you can make that decision: pass this
    # yourself, after reading the mods. Horizon's rules forbid an AI agent from supplying it.
    [switch]$TrustNativeCode,
    [ValidateSet('performance', 'release')]
    [string]$BuildProfile = 'release',
    [switch]$NoUpload
)
$ErrorActionPreference = 'Stop'

if (-not $TrustNativeCode) {
    throw 'Read the mods, then pass -TrustNativeCode yourself: they run with full rights.'
}

$Root = Split-Path -Parent $PSScriptRoot
$Launcher = "$env:LOCALAPPDATA\HorizonLauncher\horizon-launcher.exe"
$Versions = "$env:APPDATA\horizon-launcher\versions"
$Work = Join-Path $Root '.build'
$Mods = Join-Path $Work 'mods'
$Out = Join-Path $Work 'out'
$Content = Join-Path $Work 'content'
$PackSync = Join-Path $Root 'target\release\pack-sync.exe'
$Config = Join-Path $PackDir 'pack-sync.toml'
$BuildInstance = 'pack-build'
$BuilderImage = 'pack-sync-builder'
$CacheVolume = 'pack-sync-cache'
$CoreShare = 0.9
$ClientAsset = 'client-windows-x64.exe.gz'
$ServerAsset = 'server-linux-x64.tar.gz'
$ContentAsset = 'client-content.tar.gz'
$ManifestAsset = 'manifest.signed.json'

function Assert-Success([string]$What) {
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

function Get-NextSequence([string]$Repository) {
    $tags = gh release list --repo $Repository --limit 1000 --json tagName | ConvertFrom-Json
    Assert-Success 'Listing releases'
    $numbers = @($tags | ForEach-Object { if ($_.tagName -match '^pack-(\d+)$') { [int64]$Matches[1] } })
    if ($numbers.Count -eq 0) { return 1 }
    return [int64](($numbers | Measure-Object -Maximum).Maximum) + 1
}

# Build output, git internals and test or example trees are never shipped, whatever they hold.
function Find-ModFiles([string]$Source, [string]$Name) {
    $skipped = '[\\/](target|\.git|tests?|examples?|fixtures|node_modules)[\\/]'
    return @(Get-ChildItem -LiteralPath $Source -Recurse -Filter $Name |
        Where-Object { $_.FullName.Substring($Source.Length) -notmatch $skipped })
}

# The id a content mod's horizon-mod.toml declares: its top-level `id`, before any [table].
function Get-ContentId([string]$Manifest) {
    $top = ((Get-Content -Raw -LiteralPath $Manifest) -split '(?m)^\s*\[', 2)[0]
    if ($top -cnotmatch '(?m)^\s*id\s*=\s*"([a-z0-9_.-]+)"') {
        throw "$Manifest has no usable id (lowercase letters, digits, _ - .)"
    }
    return $Matches[1]
}

# Native bundles (horizon-mixin.toml) compile into the game; content mods (horizon-mod.toml with
# assets/ and data/) travel beside it in client-content.tar.gz, one folder per mod id.
function Copy-ModFiles([string]$Source) {
    $bundles = Find-ModFiles $Source 'horizon-mixin.toml'
    $contents = Find-ModFiles $Source 'horizon-mod.toml'
    if (-not $bundles -and -not $contents) { throw "No horizon-mixin.toml or horizon-mod.toml under $Source" }
    foreach ($bundle in $bundles) {
        Copy-Item -LiteralPath $bundle.Directory.FullName -Destination $Mods -Recurse
    }
    foreach ($manifest in $contents) {
        $id = Get-ContentId $manifest.FullName
        $target = Join-Path $Content $id
        if (Test-Path -LiteralPath $target) { throw "Two content mods in the pack are named $id" }
        New-Item -ItemType Directory -Force $target | Out-Null
        Copy-Item -LiteralPath $manifest.FullName -Destination $target
        foreach ($tree in 'assets', 'data') {
            $part = Join-Path $manifest.Directory.FullName $tree
            if (Test-Path -LiteralPath $part) { Copy-Item -LiteralPath $part -Destination $target -Recurse }
        }
        Write-Host "Content mod $id from $($manifest.Directory.FullName)"
    }
}

function Get-Mods($Pack) {
    foreach ($mod in $Pack.mods) {
        if ($mod.path) {
            Copy-ModFiles $mod.path
            continue
        }
        $name = $mod.repo -replace '[^A-Za-z0-9_.-]', '_'
        $checkout = Join-Path $Work "src\$name"
        git clone --quiet --depth 1 --branch $mod.tag "https://github.com/$($mod.repo).git" $checkout
        Assert-Success "Cloning $($mod.repo)"
        Copy-ModFiles $checkout
    }
}

function Get-CoreCount {
    return [int][Math]::Floor([Environment]::ProcessorCount * $CoreShare)
}

function Build-Client([string]$Base) {
    $known = & $Launcher --json instance list | ConvertFrom-Json
    if (-not ($known | Where-Object { $_.name -eq $BuildInstance })) {
        & $Launcher instance create $BuildInstance
        Assert-Success 'Creating the build instance'
    }
    $arguments = @('native-build', "`"$Mods`"", '--instance', $BuildInstance, '--version', $Base,
        '--profile', $BuildProfile, '--trust-native-code')
    $process = Start-Process $Launcher -ArgumentList $arguments -PassThru -NoNewWindow
    $null = $process.Handle
    $process.ProcessorAffinity = [IntPtr]((1L -shl (Get-CoreCount)) - 1)
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Client build failed with exit code $($process.ExitCode)" }

    $instance = & $Launcher --json instance list | ConvertFrom-Json |
        Where-Object { $_.name -eq $BuildInstance }
    $program = Join-Path $Versions "$($instance.version)\horizon-client.exe"
    if (-not (Test-Path -LiteralPath $program)) { throw "The build left no game at $program" }
    $source = [IO.File]::OpenRead($program)
    $target = [IO.File]::Create((Join-Path $Out $ClientAsset))
    $gzip = [IO.Compression.GZipStream]::new($target, [IO.Compression.CompressionLevel]::Optimal)
    try {
        $source.CopyTo($gzip)
    } finally {
        $gzip.Dispose(); $target.Dispose(); $source.Dispose()
    }
}

function Build-Server([string]$Base) {
    docker build --quiet -t $BuilderImage (Join-Path $PSScriptRoot 'docker')
    Assert-Success 'Building the Linux image'
    $prepared = Join-Path $Versions "$Base\.local"
    if (-not (Test-Path -LiteralPath $prepared)) { throw "$Base is not prepared on this computer" }
    docker run --rm --cpus (Get-CoreCount) `
        -v "${CacheVolume}:/cache" `
        -v "${Mods}:/mods:ro" `
        -v "${prepared}:/prepared:ro" `
        -v "${Out}:/out" `
        -e "BASE=$Base" -e "BUILD_PROFILE=$BuildProfile" -e 'TRUST_FLAG=--trust-native-code' `
        $BuilderImage
    Assert-Success 'Building the server'
}

if (-not (Test-Path -LiteralPath $Config)) {
    throw "$Config is missing: copy pack-sync\pack-sync.example.toml there and fill it in."
}
$Pack = Get-Content -Raw -LiteralPath (Join-Path $PackDir 'pack.json') | ConvertFrom-Json
if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
New-Item -ItemType Directory -Force $Mods, $Out | Out-Null
Get-Mods $Pack
# A trial build stays on this computer, so it does not need the repository to exist yet.
$Sequence = if ($NoUpload) { 1 } else { Get-NextSequence $Pack.repository }
Write-Host "Building pack-$Sequence on $($Pack.horizon_base)"

$env:PACK_SYNC_CONFIG = (Resolve-Path -LiteralPath $Config).Path
Push-Location $Root
try {
    cargo build --release --quiet -p pack-sync
    Assert-Success 'Building pack-sync'
} finally {
    Pop-Location
}
# Before the long builds: a wrong key would otherwise surface only at the very end.
& $PackSync check-key --key $KeyFile
Assert-Success 'Checking the signing key'
# Also before the long builds: a content folder named unlike its mod's id fails here.
$Files = @()
$ContentArguments = @()
if (Test-Path -LiteralPath $Content) {
    $ContentPath = Join-Path $Out $ContentAsset
    & $PackSync pack-content --dir $Content --out $ContentPath
    Assert-Success 'Packing the content mods'
    $Files += $ContentPath
    $ContentArguments = @('--content', $ContentPath)
}
Build-Client $Pack.horizon_base
Build-Server $Pack.horizon_base

$ClientPath = Join-Path $Out $ClientAsset
$ServerPath = Join-Path $Out $ServerAsset
$ManifestPath = Join-Path $Out $ManifestAsset
& $PackSync release --sequence $Sequence --horizon-base $Pack.horizon_base --client $ClientPath `
    --server $ServerPath @ContentArguments --key $KeyFile --out $ManifestPath
Assert-Success 'Signing'

if ($NoUpload) {
    Write-Host "Signed pack-$Sequence in $Out, not uploaded."
    return
}
gh release create "pack-$Sequence" $ManifestPath $ClientPath $ServerPath @Files `
    --repo $Pack.repository --title "Pack $Sequence" --notes "Horizon $($Pack.horizon_base)" --latest
Assert-Success 'Publishing'
Write-Host "Published pack-$Sequence. The server picks it up at its next check."
Write-Host "Give players $PackSync; it carries your settings."
