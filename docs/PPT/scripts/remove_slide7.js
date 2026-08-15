// Removes slide 7 (the "Important Pointers / Instructions" slide, which the
// template itself says to delete before submission) and every reference to
// it, so the packaged pptx has exactly 6 slides.
const fs = require('fs');
const path = require('path');

const ROOT = process.argv[2];
if (!ROOT) {
  console.error('usage: node remove_slide7.js <extracted-pptx-dir>');
  process.exit(1);
}

function rm(p) {
  const full = path.join(ROOT, p);
  if (fs.existsSync(full)) fs.unlinkSync(full);
}

// 1. Drop the slide, its own rels, and its notes slide + rels.
rm('ppt/slides/slide7.xml');
rm('ppt/slides/_rels/slide7.xml.rels');
rm('ppt/notesSlides/notesSlide6.xml');
rm('ppt/notesSlides/_rels/notesSlide6.xml.rels');

// 2. presentation.xml: drop the <p:sldId> pointing at rId8 (slide7).
{
  const p = path.join(ROOT, 'ppt/presentation.xml');
  let xml = fs.readFileSync(p, 'utf8');
  const before = xml;
  xml = xml.replace(/<p:sldId id="297" r:id="rId8"\/>/, '');
  if (xml === before) throw new Error('slide7 sldId entry not found');
  fs.writeFileSync(p, xml, 'utf8');
}

// 3. presentation.xml.rels: drop the rId8 relationship to slides/slide7.xml.
{
  const p = path.join(ROOT, 'ppt/_rels/presentation.xml.rels');
  let xml = fs.readFileSync(p, 'utf8');
  const before = xml;
  xml = xml.replace(
    /<Relationship Id="rId8" Type="[^"]*\/slide" Target="slides\/slide7\.xml"\/>/,
    '',
  );
  if (xml === before) throw new Error('rId8 relationship not found');
  fs.writeFileSync(p, xml, 'utf8');
}

// 4. [Content_Types].xml: drop the Override entries for slide7 and notesSlide6.
{
  const p = path.join(ROOT, '[Content_Types].xml');
  let xml = fs.readFileSync(p, 'utf8');
  const before = xml;
  xml = xml
    .replace(/<Override PartName="\/ppt\/slides\/slide7\.xml"[^>]*\/>/, '')
    .replace(/<Override PartName="\/ppt\/notesSlides\/notesSlide6\.xml"[^>]*\/>/, '');
  if (xml === before) throw new Error('content-type overrides not found');
  fs.writeFileSync(p, xml, 'utf8');
}

console.log('slide 7 and all references removed; deck is now 6 slides');
