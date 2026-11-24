# Runs under pwsh 7 because script/generate-licenses.ps1 does not parse in Windows PowerShell 5.1.
$start = Get-Date
pwsh -NoProfile -ExecutionPolicy Bypass -File "$PSScriptRoot\script\bundle-windows.ps1" -Channel stable @args
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
# Renamed here rather than in bundle-windows.ps1 because .github/workflows/release.yml uploads target/Wu-x86_64.exe.
Get-ChildItem "$PSScriptRoot\target\Wu-*.exe" | Where-Object { $_.LastWriteTime -ge $start } | ForEach-Object {
    Move-Item -Path $_.FullName -Destination (Join-Path $_.DirectoryName "$($_.BaseName)-stable.exe") -Force
}
