# chef one-line installer (PowerShell).
#   irm https://raw.githubusercontent.com/x87/chef/master/install.ps1 | iex
$ErrorActionPreference = "Stop"

$Repo = "x87/chef"
$ChefHome = if ($env:CHEF_HOME) { $env:CHEF_HOME } else { Join-Path $env:LOCALAPPDATA "Chef" }
$BinDir = Join-Path $ChefHome "bin"
$Asset = "chef-x86_64-pc-windows-msvc.zip"

Write-Host "resolving latest release of $Repo..."
# GitHub redirects these to the latest release's asset bytes - no API call,
# so installers are immune to api.github.com rate limits (403) and need no auth.
$BaseUrl = "https://github.com/$Repo/releases/latest/download"
$DownloadUrl = "$BaseUrl/$Asset"

$tmp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ("chef-" + [Guid]::NewGuid()))
try {
    Write-Host "downloading $Asset..."
    $zip = Join-Path $tmp $Asset
    Invoke-WebRequest $DownloadUrl -OutFile $zip

    # Verify SHA-256 against the sidecar published in the same release.
    $sidecar = (Invoke-WebRequest "$DownloadUrl.sha256").Content.Trim()
    $expected = ($sidecar -split "\s+")[0].ToLower()
    $got = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $got) { throw "checksum mismatch: expected $expected, got $got" }
    Write-Host "sha256: OK"

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Expand-Archive $zip $tmp -Force
    Remove-Item (Join-Path $BinDir "chef.old") -ErrorAction SilentlyContinue
    Move-Item (Join-Path $tmp "chef.exe") (Join-Path $BinDir "chef.exe") -Force
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

if (($env:PATH -split ";") -notcontains $BinDir) {
    Write-Host ""
    Write-Host "add chef to your PATH:"
    Write-Host "  setx PATH `"`$env:PATH;$BinDir`""
}
& (Join-Path $BinDir "chef.exe") --version