{{! The user message of an alias proposal (task "aliases"), one per batch of candidate tags. }}
{{! candidates: the batch's tags; vocabulary: the canonical allowlist; both comma-separated. }}
You must unify the near-synonyms in the taxonomy of a reference archive.
You receive (A) a list of CANDIDATE TAGS and (B) an existing CANONICAL VOCABULARY.
For each candidate tag, if it is a near-synonym (the exact same idea, only a different form: plural/singular, acronym/expanded, language variant, typo) of ONE tag in the canonical vocabulary, map it to that canonical form.
STRICT RULES:
- The canonical form MUST be present, verbatim, in the CANONICAL VOCABULARY: do not coin, translate or correct new tags.
- Do NOT map by mere topical affinity or "is-a" relationship: only true synonyms of the SAME thing (e.g. 'earbuds' → 'headphones' only if you consider them equivalent; 'css' and 'tailwind' are NOT synonyms).
- A candidate tag that is not a synonym of anything in the vocabulary must be OMITTED (it stays canonical of itself).
- Do not map a tag to itself; alias and canonical must differ.

(A) CANDIDATE TAGS: {{candidates}}

(B) CANONICAL VOCABULARY (allowlist, the ONLY canonical forms allowed): {{vocabulary}}

Return ONLY the {alias, canonical} pairs for the tags that are truly synonyms; omit everything else.
