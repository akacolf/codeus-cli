$ErrorActionPreference = "Stop"

$Repo = if ($env:CODEUS_REPO) { $env:CODEUS_REPO } else { "atomicfield/codeus-cli" }
$Version = if ($env:CODEUS_VERSION) { $env:CODEUS_VERSION } else { "latest" }
$InstallDir = if ($env:CODEUS_INSTALL_DIR) { $env:CODEUS_INSTALL_DIR } else { Join-Path $HOME ".codeus\bin" }
$BinName = if ($env:CODEUS_BIN_NAME) { $env:CODEUS_BIN_NAME } else { "codeus.exe" }

function Get-Target {
  $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
  switch ($arch) {
    "X64" { return "x86_64-pc-windows-msvc" }
    "Arm64" { return "aarch64-pc-windows-msvc" }
    default { throw "unsupported architecture: $arch" }
  }
}

function Get-BaseUrl {
  if ($Version -eq "latest") {
    return "https://github.com/$Repo/releases/latest/download"
  }
  return "https://github.com/$Repo/releases/download/$Version"
}

$Target = Get-Target
$Asset = "codeus-$Target.zip"
$BaseUrl = Get-BaseUrl
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("codeus-install-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null

try {
  $Archive = Join-Path $Tmp $Asset
  $Checksums = Join-Path $Tmp "SHA256SUMS"

  Write-Host "Downloading $Asset..."
  Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $Archive
  Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $Checksums

  $Expected = (Get-Content $Checksums | Where-Object { $_ -match "  $([regex]::Escape($Asset))$" } | Select-Object -First 1).Split(" ")[0]
  if (-not $Expected) { throw "checksum for $Asset not found" }
  $Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant()
  if ($Actual -ne $Expected.ToLowerInvariant()) { throw "checksum mismatch for $Asset" }

  Expand-Archive -Path $Archive -DestinationPath $Tmp -Force
  $Candidate = Get-ChildItem -Path $Tmp -Recurse -File -Filter "codeus*.exe" | Select-Object -First 1
  if (-not $Candidate) { throw "archive did not contain codeus.exe" }

  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  Copy-Item $Candidate.FullName (Join-Path $InstallDir $BinName) -Force

  Write-Host "Installed $BinName to $InstallDir"
  if (($env:PATH -split ";") -notcontains $InstallDir) {
    Write-Host "Add $InstallDir to PATH if codeus is not found."
  }
} finally {
  Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
