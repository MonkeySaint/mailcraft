<#
.SYNOPSIS
  Windows packages: an MSI (WiX v4) and a portable zip.

.DESCRIPTION
  packaging/windows/package.ps1 [-Target x86_64-pc-windows-msvc] [-OutDir dist] [-NoBuild]

  1. cargo build --release for mailcraft and mailcraft-cli (build details in MAILCRAFT_GIT_HASH /
     MAILCRAFT_BUILD_DATE, like packaging/env.sh does on Unix).
  2. Signs both .exe files when WINDOWS_CERT_PFX (base64 of a code-signing .pfx) and WINDOWS_CERT_PASSWORD
     are set; WINDOWS_TIMESTAMP_URL overrides the RFC 3161 timestamp server.
  3. wix build mailcraft.wxs -> mailcraft-VERSION-windows-ARCH.msi (signed the same way).
  4. mailcraft-VERSION-windows-ARCH.zip: both .exe files plus the licences.

  Needs: Rust (MSVC toolchain), git, the .NET SDK for `dotnet tool install wix` (when wix is not on PATH),
  signtool from the Windows SDK only when signing. For -Target aarch64-pc-windows-msvc, the MSVC ARM64
  build tools and clang (for the ring crate).
#>
[CmdletBinding()]
param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutDir = "",
    [switch]$NoBuild
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $OutDir) { $OutDir = Join-Path $Root "dist" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

function Invoke-Checked([string]$Exe, [string[]]$Arguments) {
    Write-Host "==> $Exe $($Arguments -join ' ')"
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed with exit code $LASTEXITCODE" }
}

# Single source of truth: APP_ID in packaging/env.sh, version in [workspace.package] of Cargo.toml.
$envSh = Get-Content (Join-Path $Root "packaging\env.sh") -Raw
if ($envSh -notmatch '(?m)^APP_ID="([^"]+)"') { throw "APP_ID not found in packaging/env.sh" }
$AppId = $Matches[1]
$manifest = Get-Content (Join-Path $Root "Cargo.toml") -Raw
if ($manifest -notmatch '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') {
    throw "no version in [workspace.package] of Cargo.toml"
}
$Version = $Matches[1]
# MSI ProductVersion is numeric only: 1.2.3-rc.1 installs as 1.2.3.
$MsiVersion = ($Version -split '[-+]')[0]

$Arch = switch -Wildcard ($Target) {
    "x86_64-*" { "x86_64" }
    "aarch64-*" { "aarch64" }
    default { throw "unsupported target $Target" }
}
$WixArch = if ($Arch -eq "aarch64") { "arm64" } else { "x64" }

if (-not $env:MAILCRAFT_GIT_HASH) {
    $env:MAILCRAFT_GIT_HASH = (& git -C $Root rev-parse --short=10 HEAD 2>$null)
    if (-not $env:MAILCRAFT_GIT_HASH) { $env:MAILCRAFT_GIT_HASH = "unknown" }
}
if (-not $env:MAILCRAFT_BUILD_DATE) {
    $env:MAILCRAFT_BUILD_DATE = (& git -C $Root log -1 --format=%cs 2>$null)
    if (-not $env:MAILCRAFT_BUILD_DATE) { $env:MAILCRAFT_BUILD_DATE = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd") }
}
Write-Host "MailCraft $Version ($AppId) for $Target, git $($env:MAILCRAFT_GIT_HASH), $($env:MAILCRAFT_BUILD_DATE)"

if (-not $NoBuild) {
    Invoke-Checked "rustup" @("target", "add", $Target)
    Invoke-Checked "cargo" @("build", "--locked", "--release", "--target", $Target, "-p", "mailcraft", "-p", "mailcraft-cli")
}
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "target" }
$BinDir = Join-Path $TargetDir "$Target\release"
foreach ($exe in @("mailcraft.exe", "mailcraft-cli.exe")) {
    if (-not (Test-Path (Join-Path $BinDir $exe))) { throw "missing $BinDir\$exe" }
}

$Work = Join-Path $TargetDir "packaging\windows-$Arch"
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force -Path $Work | Out-Null
# Sign copies, never the files in target/.
$SignedBin = Join-Path $Work "bin"
New-Item -ItemType Directory -Force -Path $SignedBin | Out-Null
Copy-Item (Join-Path $BinDir "mailcraft.exe"), (Join-Path $BinDir "mailcraft-cli.exe") $SignedBin

$Signing = [bool]($env:WINDOWS_CERT_PFX -and $env:WINDOWS_CERT_PASSWORD)
$Pfx = Join-Path $Work "cert.pfx"
function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $found = Get-ChildItem -Path $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $found) { throw "signtool.exe not found (install the Windows SDK)" }
    return $found.FullName
}
function Invoke-Sign([string[]]$Files) {
    if (-not $Signing) { return }
    $ts = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
    $tool = Find-SignTool
    foreach ($f in $Files) {
        Invoke-Checked $tool @("sign", "/f", $Pfx, "/p", $env:WINDOWS_CERT_PASSWORD, "/fd", "SHA256",
            "/tr", $ts, "/td", "SHA256", "/d", "MailCraft", $f)
    }
}

try {
    if ($Signing) {
        [IO.File]::WriteAllBytes($Pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX))
    } else {
        Write-Host "No WINDOWS_CERT_PFX/WINDOWS_CERT_PASSWORD: packages are not signed."
    }
    Invoke-Sign @((Join-Path $SignedBin "mailcraft.exe"), (Join-Path $SignedBin "mailcraft-cli.exe"))

    # WiX v4 as a .NET global tool, pinned (v5+ adds licence terms for some uses; v4 is MS-RL, build-time only).
    $wix = Get-Command wix -ErrorAction SilentlyContinue
    if (-not $wix) {
        $wixVersion = if ($env:WIX_VERSION) { $env:WIX_VERSION } else { "4.0.6" }
        Invoke-Checked "dotnet" @("tool", "install", "--global", "wix", "--version", $wixVersion)
        $env:PATH = "$env:PATH;$(Join-Path $env:USERPROFILE '.dotnet\tools')"
    }
    $Msi = Join-Path $OutDir "mailcraft-$Version-windows-$Arch.msi"
    Invoke-Checked "wix" @("build", "-arch", $WixArch, "-d", "Version=$MsiVersion", "-d", "BinDir=$SignedBin",
        "-d", "RepoDir=$Root", "-o", $Msi, (Join-Path $PSScriptRoot "mailcraft.wxs"))
    Invoke-Sign @($Msi)

    $ZipName = "mailcraft-$Version-windows-$Arch"
    $ZipDir = Join-Path $Work $ZipName
    New-Item -ItemType Directory -Force -Path (Join-Path $ZipDir "licences") | Out-Null
    Copy-Item (Join-Path $SignedBin "*.exe") $ZipDir
    foreach ($f in @("README.md", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md")) {
        Copy-Item (Join-Path $Root $f) (Join-Path $ZipDir "licences")
    }
    Copy-Item (Join-Path $Root "assets\fonts\OFL-*.txt") (Join-Path $ZipDir "licences")
    $Zip = Join-Path $OutDir "$ZipName.zip"
    if (Test-Path $Zip) { Remove-Item -Force $Zip }
    Compress-Archive -Path $ZipDir -DestinationPath $Zip
    Write-Host "==> wrote $Msi"
    Write-Host "==> wrote $Zip"
}
finally {
    if (Test-Path $Pfx) { Remove-Item -Force $Pfx }
}
