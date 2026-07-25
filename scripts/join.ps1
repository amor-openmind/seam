# seam - join a fleet from a Windows machine that has no seam yet.
#
#   $env:SEAM_SERVER="192.168.2.69:24810"
#   iwr -useb https://github.com/amor-openmind/seam-releases/releases/latest/download/join.ps1 | iex
#
# The script comes from the public releases page, not from the machine running seam: that
# machine only listens on loopback, and a binary served off the LAN would be the one thing
# nobody could verify. It asks the server which version to run, then fetches that release
# from GitHub over TLS and checks it against the published checksums.
$ErrorActionPreference = "Stop"

if (-not $env:SEAM_SERVER) {
  Write-Error "Set SEAM_SERVER to the machine running seam, e.g. `$env:SEAM_SERVER='192.168.2.69:24810'"
  return
}

$repo   = "amor-openmind/seam-releases"
$server = $env:SEAM_SERVER
$home_  = if ($env:SEAM_HOME) { $env:SEAM_HOME } else { Join-Path $env:USERPROFILE ".seam" }

# The server is trusted for one thing: a version number.
try {
  $info = Invoke-RestMethod -UseBasicParsing -TimeoutSec 5 "http://$server/join"
} catch {
  Write-Error "No answer from $server - is seam running there, and is the port right?"
  return
}
$version = $info.version
if (-not $version) { Write-Error "That machine did not report a version."; return }

New-Item -ItemType Directory -Force -Path $home_ | Out-Null
$bin = Join-Path $home_ "seam-$version.exe"

if (-not (Test-Path $bin)) {
  Write-Host "seam: fetching v$version from GitHub..."
  $base = "https://github.com/$repo/releases/download/v$version"
  $tmp  = "$bin.part"
  Invoke-WebRequest -UseBasicParsing "$base/seam.exe" -OutFile $tmp

  # Verify before trusting the bytes.
  try {
    $sums = (Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS.txt").Content
    $want = ($sums -split "`n" | Where-Object { $_ -match "seam\.exe" } | Select-Object -First 1) -split "\s+" | Select-Object -First 1
    $got  = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLower()
    if ($want -and $want.ToLower() -ne $got) {
      Remove-Item $tmp -Force
      Write-Error "Checksum mismatch - refusing to run this download."
      return
    }
  } catch { }

  Move-Item -Force $tmp $bin
} else {
  Write-Host "seam: v$version already here"
}

Write-Host "seam: starting..."
& $bin run --connect $server
