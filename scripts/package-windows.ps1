# Packages scrollback for Windows, on Windows: a zip that runs from any
# folder, and an installer where Inno Setup is there to build one.
#
#   pwsh scripts/package-windows.ps1
#
# Leaves, in target\package:
#   scrollback-<version>-windows-x64.zip
#   scrollback-<version>-windows-x64-setup.exe
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches.Groups[1].Value
$out = "target\package"
$name = "scrollback-$version-windows-x64"

cargo build --release --locked -p scrollback-app -p scrollback-cli
if ($LASTEXITCODE -ne 0) { throw "the build failed" }

Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$out\$name" | Out-Null
Copy-Item target\release\scrollback-app.exe, target\release\scrollback.exe "$out\$name\"
Compress-Archive -Path "$out\$name" -DestinationPath "$out\$name.zip"
Remove-Item -Recurse -Force "$out\$name"
Write-Host "Built $out\$name.zip"

# Inno Setup is on GitHub's Windows runners; elsewhere it may not be.
$iscc = Get-Command iscc -ErrorAction SilentlyContinue
if (-not $iscc) {
    $usual = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    if (Test-Path $usual) { $iscc = Get-Item $usual }
}
if ($iscc) {
    & $iscc.Source "/DAppVersion=$version" "/Q" packaging\windows\scrollback.iss
    if ($LASTEXITCODE -ne 0) { throw "building the installer failed" }
    Write-Host "Built $out\$name-setup.exe"
} else {
    Write-Host "Inno Setup was not found, so no installer was built."
}
