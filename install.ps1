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
    Invoke-WebRequest $DownloadUrl -OutFile $zip -UseBasicParsing

    # Verify SHA-256 against the sidecar published in the same release.
    # Note: on Windows PowerShell 5.1, Invoke-WebRequest returns .Content as
    # byte[] for application/octet-stream responses (which is what GitHub
    # serves the .sha256 sidecar as), so decode instead of calling .Trim()
    # on raw bytes. -UseBasicParsing also silences the PS 5.1 security prompt.
    $sidecarBody = (Invoke-WebRequest "$DownloadUrl.sha256" -UseBasicParsing).Content
    if ($sidecarBody -is [byte[]]) { $sidecarBody = [Text.Encoding]::UTF8.GetString($sidecarBody) }
    $expected = ($sidecarBody.Trim() -split "\s+")[0].ToLower()
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

# Add chef to the user-scope PATH (persistent, no machine-PATH changes, and no
# setx truncation at 1024 chars). Uses the user scope directly so merged
# machine+user values are never written back.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($null -eq $userPath) { $userPath = "" }
if (($userPath -split ";") -notcontains $BinDir) {
    $newPath = ($userPath.TrimEnd(';') + ';' + $BinDir).TrimStart(';')
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host ""
    Write-Host "added $BinDir to your user PATH."
    Write-Host "(takes effect in new terminal windows; this window keeps its old PATH)"
}
else {
    Write-Host "chef already on your user PATH: $BinDir"
}
& (Join-Path $BinDir "chef.exe") --version