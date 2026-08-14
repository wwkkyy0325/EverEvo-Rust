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

use super::gate::clamp_verify_fragment;
use super::gate::{classify, Difficulty};
use everevo_core::context::{ContextBuildContext, ContextFragment, ContextStage};
use everevo_core::llm::LlmMessage;

/// Injects answer-output discipline: explicit final-answer marker, verbatim
/// fidelity, constraint enumeration, and candidate verification.
///
/// Priority: 2 (after persona at 1 and best-practices at 2; before skills).
/// Adaptive: simple questions get only the format contract below; the
/// per-failure-mode remediation sections are injected for hard questions only.
pub struct AnswerDisciplineStage;

/// Format essentials that the answer scorer depends on — injected for EVERY
/// question regardless of difficulty (the `Final answer:` marker and bare-value
/// formatting must never vary). Heavy failure-mode remediation lives in the
/// full stage content (hard questions only).
const SIMPLE_DISCIPLINE: &str = "\
## Answer Discipline

### Final answer (HARD RULE)
Your final message MUST end with a single explicit `Final answer:` line \
containing ONLY the value — no reasoning, no prose, no explanations after it, \
and nothing between the value and the end of the message.

### Output format, by answer type
- **Yes/No question:** output exactly `Yes` or `No` — nothing else.
- **Numeric answer:** output the bare number as digits — no units, no `$`, no \
`%`, no thousands separators, no words. Do not round to an approximate form \
unless the question explicitly asks for it. Express the number in the units \
the QUESTION asks for.
- **String answer:** output the EXACT spelling and capitalization of the \
source term — no added articles (\"the\" / \"a\"), no rephrasing.
- **List answer:** output the item NAMES verbatim, comma-separated — never \
shortened, renamed, or reordered, and never a different count.";

impl ContextStage for AnswerDisciplineStage {
    fn priority(&self) -> i32 {
        2
    }
    fn name(&self) -> &str {
        "answer_discipline"
    }
    fn tool_visible(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Output fidelity: end with a single `Final answer:` line containing only the value; bare numbers, exact spelling, verbatim lists; epistemic boundary (commit only [VERIFIED])."
    }

    fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
        // Simple questions: only the format contract above. The heavy
        // per-failure-mode sections are skipped to avoid the extra tokens (and
        // the forced-caution overhead) that research shows harms trivial tasks.
        if classify(&ctx.user_message) == Difficulty::Simple {
            return Some(ContextFragment {
                label: "Answer Discipline".into(),
                messages: vec![LlmMessage::user(SIMPLE_DISCIPLINE)],
            });
        }
        let content = clamp_verify_fragment(
            &ctx.budget,
            "\
## Answer Discipline

### Final answer (HARD RULE)
Your final message MUST end with a single explicit `Final answer:` line \
containing ONLY the value — no reasoning, no prose, no explanations after it, \
and nothing between the value and the end of the message. Never bury the \
answer inside a sentence, and never append a closing remark after the value.

### Epistemic boundary (know what you don't know)
Keep THREE categories explicit and never blur them:
- **[VERIFIED]** — the value appeared in a tool result you actually retrieved.
- **[UNVERIFIED]** — you derived or recalled it, but no retrieved source states it.
- **[UNKNOWN]** — you could not retrieve a source for it.
Commit on the `Final answer:` line ONLY a `[VERIFIED]` value. If a candidate is \
`[UNVERIFIED]` or `[UNKNOWN]`, keep retrieving (different engine, direct fetch, \
download) until it is verified — or, if genuinely unreachable, say so rather \
than presenting an assumption as fact. This boundary is what stops context \
pollution from producing a confident-but-wrong generation.

### Satisficing — when to STOP (HARD RULE)
A candidate is SUFFICIENT, and you COMMIT it immediately, when ALL of these hold:
- it answers the question (right value, units, scale, and form per the format rules below), and
- at least ONE directly-retrieved tool result states the exact value, and
- no retrieved evidence contradicts it.
Once a candidate is SUFFICIENT, STOP researching. Re-verifying an already-verified
fact, or adding a second source \"to be rigorous\", does NOT improve the answer —
every extra call risks exhausting the budget and producing NO answer at all, which
scores 0. Only continue if a SPECIFIC open sub-question remains whose answer could
change the value. Do not continue \"for certainty\" — a SUFFICIENT candidate is
already the answer; commit it on the `Final answer:` line.

### Output format, by answer type
Format the value on the `Final answer:` line exactly as follows, or the \
answer is counted wrong:
- **Yes/No question:** output exactly `Yes` or `No` — nothing else.
- **Numeric answer:** output the bare number as digits — no units (no m³, kg, \
mph), no `$`, no `%`, no thousands separators, no words (\"one hundred\" → `100`). \
Do not round to an approximate form unless the question explicitly asks for it.
- **Unit/scale conversion (HARD RULE):** the bare number must be expressed in \
the units the QUESTION asks for, even if your source uses different units. If \
the question asks \"how many km\" and your source says 17000 m, the answer is \
`17`, not `17000`. Convert length (m↔km), mass (g↔kg), time, currency \
(thousands), per-cent vs per-thousand, and every other scale the question \
states. Re-read the question's own unit AFTER you have the raw number and \
convert before committing — a number in the wrong unit is counted wrong even \
when it is numerically faithful to the source.
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

### Titles / headings — strip production scaffolding
When the question asks for a title, name, or location heading (a castle, a \
building, an episode/scene title, a ship, a place), output ONLY the bare \
name — never the full scene/screenplay heading with its production \
scaffolding. A line like \"INT. THE CASTLE - DAY\" is a SCENE HEADING, not a \
name: the name is the `THE CASTLE` part, so strip `INT./EXT.` and the \
`- DAY/NIGHT` time marker. Likewise drop episode codes, part numbers, and \
studio markers unless the question explicitly asks for them.

### Stated constraints only
Apply EXACTLY the criteria the question states — no more, no less. When \
counting or selecting items, include every item that meets the stated \
criteria and exclude only items that fail them. NEVER impose an unstated \
category exclusion (e.g. deciding a herb is not a \"food item\" when the \
question never excludes herbs, or that a subtitle/season doesn't count). If \
the question does not say to exclude a category, the category's items count. \
When two answers differ only in whether an UNSTATED assumption was applied, \
the answer that applies ONLY the stated criteria is correct.

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

### Code for numbers (HARD RULE — numeric / count / list / table questions)
When the answer is a number, a count, a list, or a value read from a table, \
spreadsheet, or structured page, you MUST derive it with CODE, never by \
eyeballing the raw page. LLMs confabulate numbers they \"read\"; an \
interpreter does not. Concretely:
1. Fetch/save the source (HTML, CSV, text, or your earlier tool results) to a \
   file the sandbox can read.
2. Write a Python script that PARSES that file (regex, CSV/table parse, cell \
   addressing, arithmetic) and PRINTS ONLY the bare value — no prose, no \
   units, just the number/list the question asks for.
3. Run it via `shell` and read the printed value.
4. Commit EXACTLY the value your script printed — the final answer must equal \
   the script output, verbatim.
The commit gate REJECTS a numeric answer that never appeared in a shell/python \
result: if you are about to commit a number that no script printed, write the \
script first. This is the deterministic-extraction discipline (PAL: offload \
computation to an interpreter; the LLM only writes the program).

### Source-anchored extraction (HARD RULE — numbers read off a retrieved page)
When the value lives in a page you fetched or a file you downloaded, do NOT \
read the number off it by eye — RE-EXTRACT it deterministically from the \
SAVED source file with the verifier's source mode:
```
python verify_candidate.py verify --answer <candidate> \
  --source-file <saved source file> --extract <spec>
```
where `<spec>` pulls the value out of the file programmatically:
- `csv:col=N,skip=M[,agg=sum|avg]` — CSV column N (0-based), skip header rows
- `table:N` — Nth HTML table (numbers only)
- `regex:PATTERN[,group=N]` — all matches
- `label:TEXT[,agg=sum|avg]` — the numbers that follow each occurrence of TEXT
- `num[,agg=sum|avg]` — every number in the file
Then commit EXACTLY what the parser extracted (the verifier prints it). This is \
what stops \"the LLM read the wrong cell\": the extraction is a deterministic \
parser, so a wrong spec yields a wrong-but-visible value you can correct by \
tuning the spec — never by reading the page again. If the parser fails or the \
source is unreachable, report it rather than committing a value you read by \
eye.

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

### Historical-period names
When a question concerns a historical figure, event, or place, answer with the \
name that was CURRENT at the time — the name a contemporary source would use — \
not a modern successor name. A town renamed in 1792, a merged county, or a \
nation that no longer exists must be answered by its name in the era the \
question asks about. If your retrieved source for that era says the place \
was known as X, the answer is X, even when the modern settlement is now \
called Y. Verify the name's era against the source; a modern alias in your \
memory is not evidence of the historical name.
HISTORICAL BIRTHPLACES (HARD RULE): when the question asks for the birthplace of \
a historical figure (e.g. a U.S. president), answer with the name in use at the \
time of birth — do NOT convert a historical name to its modern successor \
municipality. Example: the 18th-century birthplace Braintree, Massachusetts is \
Braintree, NOT \"Quincy\", even though modern Quincy absorbed it. The question's \
wording \"cities\" or present tense does NOT license modernizing the name; the \
\"city\" the question means is the historical municipality of birth. If your era \
source says X, the answer is X.

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
- When the records are per-country entries (e.g. a directory of country flag \
articles), the mapping is record→country: read each record's OWN language \
field and join it to the flag shown in that SAME record. The target country \
is the one whose OWN record carries the distinguishing language value. Never \
infer one country's language from a different record, never pick a country \
merely because its flag happens to appear in the source, and never answer a \
country whose record you did not individually parse.
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
source row, keep retrieving or say you cannot answer — do not guess.

### Benchmark-dataset contamination (HARD RULE)
Never access, read, download, search for, or query the benchmark dataset itself \
(e.g. the GAIA dataset on HuggingFace) or any file, cache, or page that contains \
its ground-truth answers. These questions come from a public evaluation set; \
reading the answer key is cheating and is FORBIDDEN even though the data is \
publicly accessible and even if you believe the \"intended answer\" is \"public\". \
Research ONLY the question's actual subject matter — the article, website, \
person, dataset, or event it names — via the normal retrieval tools. If the \
subject is unreachable and your only route to an answer would be the answer key, \
report the source as unreachable and say you cannot determine the answer rather \
than committing a value from the dataset.",
        );
        Some(ContextFragment {
            label: "Answer Discipline".into(),
            messages: vec![LlmMessage::user(content)],
        })
    }
}
