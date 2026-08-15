# SIH2026 Idea Submission Deck

`SecureMesh-SIH2026-Idea-Presentation.pptx` is the official
[`SIH2026-IDEA-Presentation-Format (1).pptx`](SIH2026-IDEA-Presentation-Format%20(1).pptx)
template with SecureMesh's content filled in and the instructions slide (slide 7)
removed, per the template's own submission rules. Nothing in the template's
layouts, master, theme, or decorative graphics was touched — only the text
content of each shape.

**Before uploading:** three fields on the title slide are still placeholders —
`[PROBLEM STATEMENT TITLE — paste exact SIH portal wording]`, `[TEAM ID]`, and
`[TEAM NAME]` (the last also appears in a small oval on slides 2–6). Fill these
in directly in PowerPoint, or regenerate the deck (below) with the real values.

The portal only accepts PDF. Export via **File → Save As → PDF** once the
placeholders are filled in — `SecureMesh-SIH2026-Idea-Presentation.pdf` in this
folder is a preview render with the placeholders still in it, not the
submission copy.

## Regenerating

The deck is produced by editing the template's raw OOXML directly (only the
`<a:t>` text runs and bullet paragraphs inside named shapes), so the official
look is byte-identical to the template — nothing is redrawn.

```powershell
# 1. Unzip the official template
Expand-Archive "SIH2026-IDEA-Presentation-Format (1).pptx" _extract -Force
Copy-Item _extract _build -Recurse -Force

# 2. Fill in the content (edit the arrays inside build_ppt.js to change wording)
$env:SM_TEAM_NAME = "Your Team Name"
$env:SM_TEAM_ID = "Your Team ID"
$env:SM_PS_TITLE = "Exact Problem Statement Title from the SIH portal"
node scripts\build_ppt.js "$(Resolve-Path _build)"

# 3. Drop the instructions slide (the template says to delete it before submitting)
node scripts\remove_slide7.js "$(Resolve-Path _build)"

# 4. Repackage as .pptx — NOT Compress-Archive, which stores backslash path
#    separators on Windows and produces a file PowerPoint cannot open.
powershell -File scripts\zip_pptx.ps1 -SourceDir _build -DestFile SecureMesh-SIH2026-Idea-Presentation.pptx

# 5. Clean up
Remove-Item _extract, _build -Recurse -Force
```

Verified by actually opening the generated file in PowerPoint (not just by
inspecting the XML) and exporting every slide to PNG for a visual check.
