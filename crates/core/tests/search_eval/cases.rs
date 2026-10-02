//! The evaluation cases of the desktop's `scripts/search-eval/cases.ts`, with the
//! same ids, kinds, queries and gold terms. `cases_match_the_desktop_set` (in
//! the parent module) fails when the two lists drift apart.

/// How hard a case is for keyword search (`kind` in `cases.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The reported failure: no matching tag exists, the search must lean on
    /// text and keywords.
    Hard,
    /// In between.
    Mid,
    /// Already works on the desktop; guards against regressions.
    Control,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Hard, Kind::Mid, Kind::Control];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Hard => "hard",
            Kind::Mid => "mid",
            Kind::Control => "control",
        }
    }
}

/// One free-text query and its code-independent ground truth.
#[derive(Debug)]
pub struct EvalCase {
    pub id: &'static str,
    pub kind: Kind,
    pub query: &'static str,
    /// Content words (and synonyms, IT/EN): a post is relevant when its caption
    /// or AI description, keywords or tags contain one of them (`LIKE %term%`).
    pub gold_terms: &'static [&'static str],
    /// Tags known to be noise for the query. They score the desktop's AI tag
    /// retrieval (`poolRelevance`), which is not part of this evaluation.
    pub reject_tags: &'static [&'static str],
    /// Tags of the desktop's tag-only probe (`tagProbeOverride`). The tag-only
    /// ranking is not part of the FTS search; kept for parity with the set.
    pub tag_probe_override: Option<&'static [&'static str]>,
}

/// The cases, in the order of `cases.ts`. No case has a `humanGold` list.
pub const CASES: &[EvalCase] = &[
    EvalCase {
        id: "cuffie",
        kind: Kind::Hard,
        query: "Devo cercare delle reference per accessori di cuffie come ad esempio Airpods.",
        gold_terms: &[
            "cuffie",
            "cuffia",
            "airpod",
            "auricolar",
            "headphone",
            "earbud",
            "earphone",
            "over-ear",
        ],
        reject_tags: &["artefatto", "artistic", "digitalart", "designartistico"],
        tag_probe_override: None,
    },
    EvalCase {
        id: "product",
        kind: Kind::Hard,
        query: "reference di product design per gadget tecnologici e accessori indossabili",
        gold_terms: &[
            "prodotto",
            "product",
            "gadget",
            "wearable",
            "indossabil",
            "accessori",
            "device",
            "industrial",
        ],
        reject_tags: &[],
        tag_probe_override: None,
    },
    EvalCase {
        id: "tipografia",
        kind: Kind::Mid,
        query: "vorrei trovare delle reference di tipografia animata e cinetica",
        gold_terms: &[
            "typograph",
            "tipografia",
            "kinetic",
            "cinetic",
            "font",
            "lettering",
            "testo animato",
        ],
        reject_tags: &[],
        tag_probe_override: None,
    },
    EvalCase {
        id: "shader",
        kind: Kind::Control,
        query: "shader GLSL raymarching",
        gold_terms: &[
            "shader",
            "raymarch",
            "ray march",
            "glsl",
            "sdf",
            "signed distance",
        ],
        reject_tags: &[],
        tag_probe_override: None,
    },
    EvalCase {
        id: "fluidi",
        kind: Kind::Control,
        query: "simulazioni di fluidi e solver",
        gold_terms: &[
            "fluid",
            "fluido",
            "fluida",
            "solver",
            "navier",
            "simulazione fluid",
        ],
        reject_tags: &[],
        tag_probe_override: None,
    },
    EvalCase {
        id: "p8-idf",
        kind: Kind::Control,
        query: "progetti realizzati con TouchDesigner",
        gold_terms: &["touchdesigner", "touch designer"],
        reject_tags: &[],
        tag_probe_override: Some(&["touchdesigner", "effetto visivo"]),
    },
];
