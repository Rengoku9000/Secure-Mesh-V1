// Builds the SIH2026 SecureMesh submission deck from the official template,
// editing only text content inside the template's own shapes and paragraph
// formatting. No layout, master, theme, or decorative graphic is touched.
const fs = require('fs');
const path = require('path');

const ROOT = process.argv[2]; // path to the extracted pptx directory
if (!ROOT) {
  console.error('usage: node build_ppt.js <extracted-pptx-dir>');
  process.exit(1);
}

const TEAM_NAME = process.env.SM_TEAM_NAME || '[TEAM NAME]';
const TEAM_ID = process.env.SM_TEAM_ID || '[TEAM ID]';
const PS_TITLE = process.env.SM_PS_TITLE || '[PROBLEM STATEMENT TITLE — paste exact SIH portal wording]';

function readSlide(n) {
  return fs.readFileSync(path.join(ROOT, `ppt/slides/slide${n}.xml`), 'utf8');
}
function writeSlide(n, xml) {
  fs.writeFileSync(path.join(ROOT, `ppt/slides/slide${n}.xml`), xml, 'utf8');
}

function escapeXml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

// A bullet paragraph in the template's standard body style: Arial, justified,
// marL/indent "342900"/"-342900", a solid round bullet. Each bullet is a bold
// lead label followed by a normal-weight detail, matching the two-run pattern
// already used by the template's own sub-heading + body runs.
function bulletParagraph(label, detail, { size = 2000 } = {}) {
  const labelRun = label
    ? `<a:r><a:rPr lang="en-US" sz="${size}" b="1" dirty="0"><a:latin typeface="Arial" pitchFamily="34" charset="0"/><a:cs typeface="Arial" pitchFamily="34" charset="0"/></a:rPr><a:t>${escapeXml(label)}: </a:t></a:r>`
    : '';
  const detailRun = `<a:r><a:rPr lang="en-US" sz="${size}" dirty="0"><a:latin typeface="Arial" pitchFamily="34" charset="0"/><a:cs typeface="Arial" pitchFamily="34" charset="0"/></a:rPr><a:t>${escapeXml(detail)}</a:t></a:r>`;
  return `<a:p><a:pPr marL="342900" marR="0" lvl="0" indent="-342900" algn="just" defTabSz="457200" rtl="0" eaLnBrk="1" fontAlgn="base" latinLnBrk="0" hangingPunct="1"><a:lnSpc><a:spcPct val="100000"/></a:lnSpc><a:spcBef><a:spcPct val="0"/></a:spcBef><a:spcAft><a:spcPct val="20000"/></a:spcAft><a:buClrTx/><a:buSzTx/><a:buFont typeface="Arial" panose="020B0604020202020204" pitchFamily="34" charset="0"/><a:buChar char="•"/><a:tabLst/><a:defRPr/></a:pPr>${labelRun}${detailRun}</a:p>`;
}

function bulletsXml(items, opts) {
  return items.map(([label, detail]) => bulletParagraph(label, detail, opts)).join('');
}

// Replaces the <p:txBody>...</p:txBody> of the shape literally named
// `shapeName` inside one slide's XML with newParagraphsXml (a concatenation
// of <a:p> blocks), keeping the shape's own bodyPr/lstStyle wrapper.
function replaceShapeBody(slideXml, shapeName, newParagraphsXml) {
  const shapeRe = new RegExp(
    `(<p:sp>(?:(?!<p:sp>)[\\s\\S])*?name="${shapeName}"[\\s\\S]*?<p:txBody>)([\\s\\S]*?)(</p:txBody>[\\s\\S]*?</p:sp>)`,
  );
  const m = slideXml.match(shapeRe);
  if (!m) throw new Error(`shape not found: ${shapeName}`);
  // Preserve the leading <a:bodyPr .../><a:lstStyle/> that opens every txBody.
  const bodyPrMatch = m[2].match(/^(<a:bodyPr[^>]*(?:\/>|>[\s\S]*?<\/a:bodyPr>))(<a:lstStyle\/>)/);
  const prefix = bodyPrMatch ? bodyPrMatch[1] + bodyPrMatch[2] : '<a:bodyPr wrap="square"><a:spAutoFit/></a:bodyPr><a:lstStyle/>';
  return slideXml.replace(shapeRe, `$1${prefix}${newParagraphsXml}$3`);
}

function replaceAllText(xml, from, to) {
  const escaped = from.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return xml.replace(new RegExp(`<a:t>${escaped}</a:t>`, 'g'), `<a:t>${escapeXml(to)}</a:t>`);
}

// ---------------------------------------------------------------------------
// Slide 1 — title page
// ---------------------------------------------------------------------------
{
  let xml = readSlide(1);
  xml = replaceAllText(xml, 'Problem Statement ID –', `Problem Statement ID – STH260001`);
  xml = replaceAllText(xml, 'Problem Statement Title-', `Problem Statement Title- ${PS_TITLE}`);
  xml = replaceAllText(xml, 'Theme-', 'Theme- Blockchain & Cybersecurity');
  xml = replaceAllText(xml, 'PS Category- Software/Hardware', 'PS Category- Hardware');
  xml = replaceAllText(xml, 'Team ID-', `Team ID- ${TEAM_ID}`);
  xml = replaceAllText(xml, 'Team Name (Registered on portal)', `Team Name- ${TEAM_NAME}`);
  writeSlide(1, xml);
}

// ---------------------------------------------------------------------------
// Slide 2 — IDEA TITLE (Proposed Solution)
// ---------------------------------------------------------------------------
{
  let xml = readSlide(2);
  const bullets = bulletsXml(
    [
      ['What it is', 'A self-contained, offline-first edge node for comms, storage, sync and local AI — zero cloud dependency.'],
      ['Mesh', 'Local peer discovery (mDNS) + Ed25519/QUIC auth + an append-only signed event log — converges correctly across partitions, no blockchain overhead.'],
      ['Trust', 'Every peer is explicitly enrolled (UNKNOWN → PENDING → TRUSTED → REVOKED); enforced in the Rust core, never the frontend.'],
      ['Local AI', 'On-device Qwen2.5-1.5B + BGE embeddings via llama.cpp — zero API keys, zero network calls, citation-verified answers.'],
      ['Proof, not pitch', '499 automated tests, a working Tauri desktop app, and measured — not invented — evaluation numbers.'],
    ],
    { size: 2000 },
  );
  xml = replaceShapeBody(xml, 'TextBox 8',
    `<a:p><a:pPr marL="342900" indent="-342900"><a:buFont typeface="Wingdings" panose="05000000000000000000" pitchFamily="2" charset="2"/><a:buChar char="v"/></a:pPr><a:r><a:rPr lang="en-US" sz="3200" b="1" u="sng" dirty="0"><a:solidFill><a:schemeClr val="tx2"/></a:solidFill><a:latin typeface="Arial" pitchFamily="34" charset="0"/><a:cs typeface="Arial" pitchFamily="34" charset="0"/></a:rPr><a:t>Proposed Solution</a:t></a:r></a:p>${bullets}`,
  );
  xml = replaceAllText(xml, 'Your Team Name', TEAM_NAME);
  writeSlide(2, xml);
}

// ---------------------------------------------------------------------------
// Slide 3 — TECHNICAL APPROACH
// ---------------------------------------------------------------------------
{
  let xml = readSlide(3);
  const bullets = bulletsXml(
    [
      ['Core', 'Rust (Tauri 2) backend; React + TypeScript frontend; SQLite with versioned migrations.'],
      ['Identity', 'Ed25519 keypairs; node ID = SHA-256(public key); private key never leaves the core, never logged.'],
      ['Networking', 'libp2p over QUIC; mDNS discovery; TLS handshake reuses the node’s own signing key — no custom crypto.'],
      ['Data model', 'Append-only signed event log, per-origin monotonic sequence numbers; conflicts flagged, never silently overwritten.'],
      ['Local AI / RAG', 'llama.cpp (CPU-only) running Qwen2.5-1.5B-Instruct + BGE-small-en-v1.5; schema-constrained JSON output; vector search over SQLite.'],
      ['Methodology', 'Test-first — 499 automated tests, clippy-clean, deterministic synthetic datasets, a reproducible AI benchmark harness.'],
    ],
    { size: 1800 },
  );
  xml = replaceShapeBody(xml, 'TextBox 8', bullets);
  xml = replaceAllText(xml, 'Your Team Name', TEAM_NAME);
  writeSlide(3, xml);
}

// ---------------------------------------------------------------------------
// Slide 4 — FEASIBILITY AND VIABILITY
// ---------------------------------------------------------------------------
{
  let xml = readSlide(4);
  const bullets = bulletsXml(
    [
      ['Feasible today', 'Core mesh + local AI are already implemented and measured across 5 completed phases — not a concept.'],
      ['Commodity hardware', 'Runs on an ordinary laptop CPU (~2.5s per incident analysis, no GPU required) — realistic for field-deployable nodes.'],
      ['Risk — model accuracy', 'Small on-device models can misclassify; the model’s output is advisory and never overwrites an operator’s own severity rating.'],
      ['Risk — revocation', 'A trust decision is local today; Phase 2.75 adds signed revocation propagation across the mesh.'],
      ['Risk — key storage', 'The private key is currently a protected file, not hardware-backed; a hardware feasibility study (Jetson Orin + TrustZone/OP-TEE) already maps the migration path.'],
      ['Strategy', 'Every claim in this deck is backed by a passing test or a measured number — the same discipline scales toward deployment.'],
    ],
    { size: 1700 },
  );
  xml = replaceShapeBody(xml, 'TextBox 8', bullets);
  xml = replaceAllText(xml, 'Your Team Name', TEAM_NAME);
  writeSlide(4, xml);
}

// ---------------------------------------------------------------------------
// Slide 5 — IMPACT AND BENEFITS
// ---------------------------------------------------------------------------
{
  let xml = readSlide(5);
  const bullets = bulletsXml(
    [
      ['Who benefits', 'Disaster-response teams, defence/field units, and remote operations in low- or no-connectivity environments.'],
      ['Continuity when networks fail', 'Incident capture, sync and AI-assisted triage keep working exactly when centralised systems go dark.'],
      ['Security by default', 'No cloud dependency means no exfiltration surface, no vendor key, no dependence on infrastructure the team doesn’t control.'],
      ['Faster, trustworthy decisions', 'Local AI turns raw field reports into classified, cited incidents in ~2.5 seconds — no data centre, no wait.'],
      ['Auditable, not blind, trust', 'Every peer, event and trust decision is signed and independently verifiable — no black-box central authority.'],
      ['Economic', 'Runs on commodity edge hardware; no recurring cloud or API cost.'],
    ],
    { size: 1700 },
  );
  xml = replaceShapeBody(xml, 'TextBox 8', bullets);
  xml = replaceAllText(xml, 'Your Team Name', TEAM_NAME);
  writeSlide(5, xml);
}

// ---------------------------------------------------------------------------
// Slide 6 — RESEARCH AND REFERENCES
// ---------------------------------------------------------------------------
{
  let xml = readSlide(6);
  const bullets = bulletsXml(
    [
      ['Project repository', 'github.com/Rengoku9000/Secure-Mesh-V1 — source, docs, tests and measured benchmarks.'],
      ['Qwen2.5-1.5B-Instruct', 'huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF (Apache-2.0).'],
      ['BGE-small-en-v1.5', 'huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf (MIT).'],
      ['llama.cpp', 'github.com/ggml-org/llama.cpp (MIT), build b10375, CPU inference runtime.'],
      ['libp2p', 'libp2p.io — peer-to-peer networking stack (QUIC transport, mDNS discovery).'],
      ['NVIDIA Jetson Linux Developer Guide', 'docs.nvidia.com/jetson — OP-TEE, Secure Boot, Firmware TPM.'],
      ['OP-TEE', 'optee.readthedocs.io — open-source TrustZone trusted execution environment.'],
    ],
    { size: 1600 },
  );
  xml = replaceShapeBody(xml, 'TextBox 8', bullets);
  xml = replaceAllText(xml, 'Your Team Name', TEAM_NAME);
  writeSlide(6, xml);
}

console.log('slides 1-6 rewritten');
