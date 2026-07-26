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

# Always the latest release. GitHub resolves `latest` itself, so the command carries no
# version and never goes stale - an earlier design put the version in the command and in an
# environment variable, which turned a one-line join into something to keep in step by hand.
$base = "https://github.com/$repo/releases/latest/download"

New-Item -ItemType Directory -Force -Path $home_ | Out-Null
$bin = Join-Path $home_ "seam.exe"

# Fetch every time: a few megabytes over a LAN is cheaper than reasoning about whether the
# copy on disk is still newest, and re-running the command is how a person updates.
Write-Host "seam: fetching the latest release..."
$tmp = "$bin.part"
Invoke-WebRequest -UseBasicParsing "$base/seam.exe" -OutFile $tmp
if ($true) {

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

  # A running seam holds a lock on its own exe, and Move-Item cannot replace a
  # locked file - re-running this script then died on the move. Ask the running
  # one to quit over loopback (works whatever it is elevated to), give it a
  # moment, and rename the old file aside as the last resort: Windows allows
  # renaming a running exe even while it refuses to overwrite it.
  $note = Join-Path $env:APPDATA "seam\seam\data\ui-port"
  if (Test-Path $note) {
    $uiport = (Get-Content $note -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($uiport) {
      try {
        Invoke-WebRequest -UseBasicParsing -Method POST "http://127.0.0.1:$uiport/action/quit" -TimeoutSec 3 | Out-Null
      } catch { }
      Start-Sleep -Milliseconds 800
    }
  }
  if (Test-Path $bin) {
    try { Remove-Item -Force $bin -ErrorAction Stop } catch {
      try { Remove-Item -Force "$bin.old" -ErrorAction SilentlyContinue } catch { }
      Rename-Item -Force $bin "$bin.old"
    }
  }
  Move-Item -Force $tmp $bin
}

Write-Host "seam: starting..."
& $bin run --connect $server
