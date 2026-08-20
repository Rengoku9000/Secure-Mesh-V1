//! The operational knowledge pack: field guidance that ships with the node.
//!
//! # Why this exists
//!
//! Before this, the index held only incidents, so a node with no incidents
//! could answer nothing. "What should I do in heavy rain?" returned the refusal
//! — correctly, since nothing local supported an answer, but uselessly. A field
//! assistant that can only recite what has already been reported is a search
//! box, not an assistant.
//!
//! This pack gives retrieval something to find on a node that has just been
//! provisioned. It does not change how answers are produced: the same BGE
//! embedding, the same threshold, the same Qwen generator, the same refusal
//! when nothing is relevant. Only the corpus is larger.
//!
//! # Where the text comes from
//!
//! The documents are compiled into the binary with `include_str!`. There is no
//! download, no filesystem lookup at runtime, and no path an operator has to
//! populate — the pack is present exactly when the binary is, which is the only
//! honest way to promise it works offline.
//!
//! # What the text is
//!
//! **Demonstration content written for this project.** It is not sourced from
//! NDMA, NDRF, FEMA, the IFRC or any other authority, and it is not agency
//! doctrine. Every document says so in its own first lines, so a passage that
//! reaches an operator after chunking still carries the disclaimer with it — a
//! label that only lives in a database column would be stripped by the very
//! pipeline that quotes the text.
//!
//! On a real device this is the slot a genuine licensed procedure set drops
//! into at provisioning time. The mechanism is the deliverable.

use serde::Serialize;

/// The `source_type` recorded against every document in this pack.
///
/// Retrieval reads it back to label a passage, which is how an operator can
/// tell standing guidance from a live report in the same answer.
pub const SOURCE_TYPE: &str = "OPERATIONAL_KNOWLEDGE";

/// Named so a corpus audit can tell which build produced a document, and so a
/// revised pack is installable alongside rather than silently conflicting.
pub const PACK_VERSION: &str = "securemesh-demo-operational-v1";

/// One document in the pack.
pub struct PackDocument {
    /// Shown to the operator as the citation.
    pub title: &'static str,
    /// The file this came from, so a citation can be traced to source.
    pub source: &'static str,
    pub text: &'static str,
}

/// Every document in the pack, in the order they are installed.
pub const DOCUMENTS: &[PackDocument] = &[
    PackDocument {
        title: "Heavy Rain Response",
        source: "heavy_rain_response.md",
        text: include_str!("../../../knowledge/emergency/heavy_rain_response.md"),
    },
    PackDocument {
        title: "Flood Response",
        source: "flood_response.md",
        text: include_str!("../../../knowledge/emergency/flood_response.md"),
    },
    PackDocument {
        title: "Avalanche Response",
        source: "avalanche_response.md",
        text: include_str!("../../../knowledge/emergency/avalanche_response.md"),
    },
    PackDocument {
        title: "Landslide Response",
        source: "landslide_response.md",
        text: include_str!("../../../knowledge/emergency/landslide_response.md"),
    },
    PackDocument {
        title: "Earthquake Response",
        source: "earthquake_response.md",
        text: include_str!("../../../knowledge/emergency/earthquake_response.md"),
    },
    PackDocument {
        title: "Lightning Safety",
        source: "lightning_response.md",
        text: include_str!("../../../knowledge/emergency/lightning_response.md"),
    },
    PackDocument {
        title: "Extreme Wind Response",
        source: "extreme_wind_response.md",
        text: include_str!("../../../knowledge/emergency/extreme_wind_response.md"),
    },
    PackDocument {
        title: "Evacuation",
        source: "evacuation_response.md",
        text: include_str!("../../../knowledge/emergency/evacuation_response.md"),
    },
    PackDocument {
        title: "Road Blockage Response",
        source: "road_blockage_response.md",
        text: include_str!("../../../knowledge/emergency/road_blockage_response.md"),
    },
    PackDocument {
        title: "Communications Failure",
        source: "communication_failure.md",
        text: include_str!("../../../knowledge/emergency/communication_failure.md"),
    },
    PackDocument {
        title: "Field Team Safety",
        source: "field_team_safety.md",
        text: include_str!("../../../knowledge/emergency/field_team_safety.md"),
    },
];

/// What an installation did.
///
/// Reports "already present" separately from "installed" rather than reporting
/// a total, so a second installation visibly does nothing instead of looking
/// like it worked twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub documents_installed: usize,
    pub documents_already_present: usize,
    pub chunks_created: u64,
}

impl InstallReport {
    /// True when everything in the pack was already in the index.
    pub fn was_already_installed(&self) -> bool {
        self.documents_installed == 0 && self.documents_already_present == DOCUMENTS.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_document_has_content_and_a_distinct_title() {
        let mut titles: Vec<&str> = DOCUMENTS.iter().map(|d| d.title).collect();
        titles.sort_unstable();
        let count = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), count, "two documents share a title");

        for document in DOCUMENTS {
            assert!(
                document.text.len() > 500,
                "{} is too short to be useful",
                document.title
            );
            assert!(document.source.ends_with(".md"));
        }
    }

    #[test]
    fn every_document_labels_itself_as_demonstration_content() {
        // The label has to survive chunking, so it lives in the text rather
        // than only in a column. A passage quoted back to an operator must
        // still say what it is.
        for document in DOCUMENTS {
            assert!(
                document
                    .text
                    .contains("SecureMesh Demo Operational Knowledge"),
                "{} does not label itself",
                document.title
            );
            assert!(
                document.text.contains("not official"),
                "{} does not disclaim official status",
                document.title
            );
        }
    }

    #[test]
    fn no_document_claims_an_authority_it_does_not_have() {
        // Naming a real agency in guidance this project wrote would be a
        // fabricated attribution, whatever the surrounding text said.
        // Whole words, matched case-sensitively. Both matter: a substring
        // search finds "FEMA" inside nothing useful but does find "WHO" inside
        // ordinary prose, and case-folding makes the pronoun "who" and the
        // agency indistinguishable. An acronym is uppercase by definition, so
        // comparing as written separates them exactly.
        for document in DOCUMENTS {
            let words: Vec<&str> = document
                .text
                .split(|c: char| !c.is_ascii_alphanumeric())
                .collect();

            for authority in ["NDMA", "NDRF", "FEMA", "IFRC", "WHO", "UNICEF", "OSHA"] {
                assert!(
                    !words.contains(&authority),
                    "{} references {authority}",
                    document.title
                );
            }
            for name in ["United Nations", "World Health Organization", "Red Cross"] {
                assert!(
                    !document.text.contains(name),
                    "{} references {name}",
                    document.title
                );
            }
        }
    }
}
