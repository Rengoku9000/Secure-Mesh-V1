param(
    [Parameter(Mandatory=$true)][string]$SourceDir,
    [Parameter(Mandatory=$true)][string]$DestFile
)

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (Test-Path $DestFile) { Remove-Item $DestFile -Force }

$SourceDir = (Resolve-Path $SourceDir).Path
$fsMode = [System.IO.FileMode]::Create
$stream = New-Object System.IO.FileStream($DestFile, $fsMode)
$archive = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)

# [Content_Types].xml first, by OPC convention (not strictly required, but
# matches how real Office-produced packages are laid out).
$files = @(Get-ChildItem -Path $SourceDir -Recurse -File -Force)
$contentTypes = $files | Where-Object { $_.Name -eq '[Content_Types].xml' }
$rest = $files | Where-Object { $_.Name -ne '[Content_Types].xml' }
$ordered = @($contentTypes) + @($rest)

foreach ($file in $ordered) {
    $relative = $file.FullName.Substring($SourceDir.Length + 1) -replace '\\', '/'
    $entry = $archive.CreateEntry($relative, [System.IO.Compression.CompressionLevel]::Optimal)
    $entryStream = $entry.Open()
    $bytes = [System.IO.File]::ReadAllBytes($file.FullName)
    $entryStream.Write($bytes, 0, $bytes.Length)
    $entryStream.Close()
}

$archive.Dispose()
$stream.Dispose()

Write-Output "wrote $DestFile"
