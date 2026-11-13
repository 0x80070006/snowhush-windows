# Construit dist\setup.msi (Snowhush pour Windows).
# Prérequis : Rust, et WiX 3.14 dans tools\wix (candle.exe, light.exe).
# Usage : powershell -ExecutionPolicy Bypass -File .\build-msi.ps1

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$wix  = Join-Path $root 'tools\wix'
$obj  = Join-Path $root 'target\wix'
$dist = Join-Path $root 'dist'

foreach ($t in 'candle.exe', 'light.exe') {
    if (-not (Test-Path (Join-Path $wix $t))) {
        throw "WiX introuvable : $wix\$t (télécharger wix314-binaries.zip depuis github.com/wixtoolset/wix3 et l'extraire dans tools\wix)"
    }
}

# Version lue dans Cargo.toml (x.y.z).
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Snowhush $version"

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build a échoué" }
} finally { Pop-Location }

$exe  = Join-Path $root 'target\release\snowhush.exe'
New-Item -ItemType Directory -Force $obj, $dist | Out-Null
$ico  = Join-Path $obj 'snowhush.ico'
# Application graphique : PowerShell n'attend pas sa fin sans -Wait.
Remove-Item $ico -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $exe -ArgumentList '--export-icon', "`"$ico`"" -Wait -PassThru
if ($p.ExitCode -ne 0 -or -not (Test-Path $ico)) { throw "export de l'icône impossible" }

$wxs = Join-Path $root 'installer\snowhush.wxs'
$lic = Join-Path $root 'installer\License.rtf'
& (Join-Path $wix 'candle.exe') -nologo -arch x64 -ext WixUtilExtension -ext WixUIExtension `
    "-dVersion=$version" "-dExePath=$exe" "-dIconPath=$ico" "-dLicensePath=$lic" `
    -out "$obj\" $wxs
if ($LASTEXITCODE -ne 0) { throw "candle a échoué" }

$msi = Join-Path $dist 'setup.msi'
# ICE91 : avertissement attendu pour une installation par utilisateur.
& (Join-Path $wix 'light.exe') -nologo -ext WixUtilExtension -ext WixUIExtension -cultures:fr-FR `
    -sice:ICE91 -spdb -out $msi (Join-Path $obj 'snowhush.wixobj')
if ($LASTEXITCODE -ne 0) { throw "light a échoué" }

$size = (Get-Item $msi).Length / 1KB
Write-Host ("OK : {0} ({1:N0} Ko)" -f $msi, $size)
