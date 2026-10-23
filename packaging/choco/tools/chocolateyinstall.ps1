$ErrorActionPreference = 'Stop'

$version  = '0.0.3'
$url      = "https://github.com/kidchenko/cantos/releases/download/v$version/Cantos-Setup-$version.exe"

$packageArgs = @{
  packageName    = 'cantos'
  fileType       = 'EXE'
  url64bit       = $url
  # Replaced by the release workflow, which computes it from the built artifact.
  checksum64     = 'REPLACE_WITH_SHA256'
  checksumType64 = 'sha256'
  # Inno Setup silent switches. /NORESTART because nothing here needs a reboot.
  silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-'
  validExitCodes = @(0)
  softwareName   = 'Cantos*'
}

Install-ChocolateyPackage @packageArgs
