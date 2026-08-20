$ProjectRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$Binary = Join-Path $ProjectRoot "dist-app\securemesh.exe"

if (-not (Test-Path $Binary)) {
    Write-Host "Binary not found at $Binary. Staging..." -ForegroundColor Yellow
    Set-Location $ProjectRoot
    cmd.exe /c "npm run app:stage"
}

Write-Host "Launching Node A (smA)..." -ForegroundColor Cyan
Start-Process powershell -ArgumentList "-NoExit", "-Command", "`$env:SECUREMESH_DATA_DIR='$env:TEMP\smA'; & '$Binary'"

Start-Sleep -Seconds 2

Write-Host "Launching Node B (smB)..." -ForegroundColor Cyan
Start-Process powershell -ArgumentList "-NoExit", "-Command", "`$env:SECUREMESH_DATA_DIR='$env:TEMP\smB'; & '$Binary'"

Write-Host "Both nodes launched in separate interactive windows." -ForegroundColor Green
