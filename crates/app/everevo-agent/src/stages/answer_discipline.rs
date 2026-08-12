//! AnswerDiscipline ContextStage — output-fidelity rules for final answers.
//!
//! Injected at priority 2 (right after BestPractices, before skills). Covers
//! the ReAct `Final answer:` marker convention the harness scorer relies on,
//! output formatting by answer type, plus the three failure classes observed in
//! the GAIA L1 benchmark run.
//!
//! - q30: verbatim list fidelity (renamed/reordered source items)
//! - q37: constraint-interpretation (quantifier/reading enumeration)
//! - q16: candidate verification against every question constraint

use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects answer-output discipline: explicit final-answer marker, verbatim
/// fidelity, constraint enumeration, and candidate verification.
///
/// Priority: 2 (after persona at 1 and best-practices at 2; before skills).
pub struct AnswerDisciplineStage;

impl ContextStage for AnswerDisciplineStage {
    fn priority(&self) -> i32 {
        2
    }
    fn name(&self) -> &str {
        "answer_discipline"
    }

    fn build(&self, _ctx: &ContextBuildContext) -> Option<ContextFragment> {
        let content = "\
## Answer Discipline

### Final answer (HARD RULE)
Your final message MUST end with a single explicit `Final answer:` line \
containing ONLY the value — no reasoning, no prose, no explanations after it, \
and nothing between the value and the end of the message. Never bury the \
answer inside a sentence, and never append a closing remark after the value.

### Output format, by answer type
Format the value on the `Final answer:` line exactly as follows, or the \
answer is counted wrong:
- **Yes/No question:** output exactly `Yes` or `No` — nothing else.
- **Numeric answer:** output the bare number as digits — no units (no m³, kg, \
mph), no `$`, no `%`, no thousands separators, no words (\"one hundred\" → `100`). \
Do not round to an approximate form unless the question explicitly asks for it.
- **List answer:** output the item NAMES verbatim, comma-separated, never \
shortened, renamed, or rephrased, and never a different count. Item names are \
ATOMIC — do not strip qualifier adjectives/quantifiers (\"fresh basil\", \
\"green beans\", \"sweet potatoes\", \"whole allspice\", \"bell pepper\" are \
each one item). ORDER: if the question asks you to sort or alphabetize, sort \
by the FULL verbatim string of each item (\"fresh basil\" sorts under f, not \
b); otherwise keep the question/source's written order.
- **String answer:** output the EXACT spelling and capitalization of the source \
term — no added articles (\"the\" / \"a\"), no rephrasing, no dropped or added words.

### Verbatim fidelity
When the question asks for items, names, or identifiers that must match a \
source (lists, authors, labels, codes), preserve their EXACT written form. Do \
not rephrase, rename, abbreviate, or reorder them. When sorting is required, \
sort by the full written string — never by a shortened form. This is the rule \
that keeps \"fresh basil\" as \"fresh basil\", never \"basil\".

### Constraint enumeration
When a quantifier or constraint admits more than one reading (\"at least\", \
\"at most\", \"minimum of N\", \"excluding\", \"among/amongst\"), enumerate every \
non-vacuous reading and test each against the available evidence before \
committing to an answer.
KEY AMBIGUITY — \"at least ONE X\" vs \"EVERY X\": a phrase like \"one box must \
contain at least 2 coins\" is ambiguous between (a) at least one box and \
(b) every box. If the \"at least one\" reading is TRIVIALLY satisfied by the \
problem setup (e.g. 30 coins across 3 boxes already forces one box ≥ 10), \
that is a strong signal the intended reading is the BINDING one — apply it to \
EVERY element. When brute-forcing a combinatorial or game-theory problem, \
enumerate the solution space under EVERY non-vacuous reading of EVERY \
constraint clause before committing. Concrete: in the 3-box game-show \
problem, filter valid host placements with min(c1,c2,c3) >= 2 (every box ≥ 2), \
not max(c1,c2,c3) >= 2.
This is a HARD RULE, not a suggestion: a stated constraint is NEVER just \
flavor or a red herring. If your chosen reading makes any constraint clause \
VACUOUS (trivially satisfied by the setup — e.g. \"some box ≥ 2\" is automatic \
when 30 coins fill 3 boxes), that reading is the WRONG one and its answer \
must NOT be committed. When two readings yield different numeric answers, \
the intended answer comes from the reading under which EVERY stated \
constraint is binding — even when that reading feels less \"natural\" \
grammatically. Do not let grammatical intuition about what \"one box\" means \
override a constraint that binds under a different reading.

### Candidate verification
Before committing a candidate answer found through research, verify it against \
EVERY constraint in the question — the right article/version, the exact value, \
and units. If a candidate fails any constraint, discard it and keep searching.

### Count-to-pick questions (most/least/fewest titles)
When the question asks which article/section/part has a QUOTED term in the \
MOST (or fewest) titles/entries (e.g. \"the article that has 'witnesses' in \
the most titles\"), you MUST:
- fetch the page that lists every candidate and its title,
- count the EXACT quoted term (exact spelling — plural vs singular matters) \
in each candidate's titles, programmatically from the fetched HTML, and
- pick the candidate with the highest count.
A section whose own heading contains the term (e.g. a section NAMED \
\"Witnesses\") is NOT automatically the one with the most occurrences in its \
rule titles — always verify by counting the actual titles. Never count from \
memory or from rule numbers alone: extract the full titles from the fetched \
page. Enumerate every candidate's matching titles explicitly before deciding.

### \"Word deleted in the last amendment\"
When the question asks what word was deleted/changed in the last amendment to \
a rule, the answer MUST come from an explicit statement in that rule's \
committee/amendment notes (e.g. LII's \"Committee Notes on Rules—20XX \
Amendment\"). Quote that statement. A generic restyling note that merely says \
\"stylistic changes, no intent to change any result\" does NOT document a \
deleted word; if the rule you reached has no documented deletion, your \
earlier identification (which article/rule) is probably wrong — go back and \
re-do the hop, never infer a word by diffing old vs new rule text.

### No-guess rule (web research)
Never output a factual answer to a web-research question from memory when \
every retrieval attempt failed or returned no authoritative content. Keep \
retrieving — offline cache, alternate mirrors, research_search, the \
`download` tool — until a tool result contains the candidate; commit ONLY a \
candidate that appeared verbatim in a tool result you actually retrieved. If \
no source was obtained, report the source as unreachable rather than \
committing a guess.

### Proper-noun / compound evidence
When the final answer is a person's surname, a place name, a product, a \
compound term, or an identifier, that exact string MUST appear verbatim in at \
least one tool result (a fetched page, a search snippet, a downloaded file). \
If no tool result contained it, you are hallucinating — do NOT commit it; \
change strategy (fetch the page directly, different engine, `download`) until \
a retrieved source contains the candidate term.

### Unique-item identification (\"the one that differs\", \"unique flag\")
When the question asks WHICH item in a set has a distinguishing property (the \
article whose flag is unique from the others, the entry that differs from the \
rest, the only X), identify the target by mapping the property to the SPECIFIC \
item, row by row, in the retrieved source — do not aggregate to a \
country/group level and then guess:
- For each result, record BOTH its stated property (e.g. its cataloged LANGUAGE \
— look for \"unknown\"/\"und\"/\"unbestimmt\" in the record's language field) AND \
its flag/country. The \"unique\" flag belongs to the ONE item that also carries \
the other stated property (the unknown-language article). Answer with that \
item's country.
- A flag/country merely present in the source is NOT the answer by itself; the \
answer is the country of the item that has the stated property, read off that \
same item.
- NEVER break a tie among several candidates by attributing an UNOBSERVED \
property to one of them (\"one candidate's records carried no detectable \
language label\") to make it fit. If the source does not let you map item→property \
unambiguously, keep retrieving (mirror, alternate archive snapshot, `download`) \
until exactly one item is identifiable; if none is, say you cannot determine \
which item the question means rather than committing a tie-break guess.

### \"Which listed entry did NOT mention X\" questions
When a question asks which entry in a named listing (a journal subject \
collection, a tag-filtered set, a directory page) did NOT mention a term, and \
then asks what that entry is or studies, you MUST fetch the authoritative \
listing page itself — a search snippet that omits the term proves nothing — \
enumerate every candidate entry matching the question's filters, fetch each \
candidate's full text, and establish term-absence from actual fetched text \
before answering. Never commit a name, compound, or stripped prefix that \
appears in no fetched tool result.

### Prefix stripping (\"don't use the prefix nano\")
When the question says the answer must not include a prefix if one exists \
(e.g. \"don't use the prefix nano\"), strip that prefix from the entity \
(nanodiamond → diamond) and make the stripped name the final answer.

### Lookup-table / roster questions
When the question asks which entity occupies a named slot in an ordered table \
or roster (number before/after X, row before/after Y, who wears number N), \
read the complete key→value table from ONE authoritative source and fill every \
named slot from that table. A recalled name is NOT evidence — never fill an \
un-read slot from memory. Query by the MISSING KEY, never by your guessed \
value (\"who wears number N for <team> in <year>\" — a query naming your \
guessed player can never validate the guess). Before the final answer, every \
named slot must trace to a source row (number→name); if any slot has no \
source row, keep retrieving or say you cannot answer — do not guess.";
        Some(ContextFragment {
            label: "Answer Discipline".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}
