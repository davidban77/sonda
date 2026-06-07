# Sonda docs style guide

This guide is the source of truth for Sonda's user-facing documentation voice. It is internal — not published on the site. Implementer agents writing or rewriting any page under `docs/site/docs/**` must follow these rules and run the checklist in Section 10 before reporting work.

The model we copy is the **FastAPI tutorial**: [https://fastapi.tiangolo.com/tutorial/first-steps/](https://fastapi.tiangolo.com/tutorial/first-steps/). Read one of its pages before you write. Notice short sentences, vocabulary introduced in prose before code, and admonitions for tangents.

## 1. Audience

The primary reader is a **network observability or SRE engineer who is not a native English speaker and not based in the United States**. They are technically experienced — they know Prometheus, alerts, scrape models, log shippers. They are not US-tech-fluent. Slang like "ship it," "spin up," "out of the box," "sugar," or "swallowed during the for: clause" filters them out, even when they understand the underlying concept.

Write for someone who learned English from textbooks, RFCs, and product documentation — not from Hacker News, Twitter, or US startup blogs. If a sentence requires US-tech culture to parse, rewrite it.

Self-check: would a senior SRE in São Paulo, Bangalore, or Madrid read this sentence once and understand it? If they need to re-read it, the sentence has failed.

## 2. Voice principles

1. **Pedagogy over personality.** The docs teach. They do not entertain. A boring sentence that lands the concept is better than a clever sentence that requires re-reading.
2. **Direct over clever.** State the fact. Skip the warm-up. Skip the metaphor unless the metaphor is the shortest path.
3. **Vocabulary before code.** Every Sonda term (`scenario`, `generator`, `encoder`, `sink`, `defaults`, `kind: runnable`) must be defined in prose before it appears in a code block on that page.
4. **One concept per paragraph, one concept per code block.** If a paragraph teaches two things, split it. If a code block introduces two new ideas, split it into two blocks.
5. **Tangents go in admonitions.** Do not bury caveats and asides inside the main sentence with em-dashes and parentheticals. Lift them into `!!! note` or `!!! tip`.

## 3. Sentence-level rules

These rules are countable. Run them on every paragraph you write.

| Rule | Limit | How to check |
|---|---|---|
| Explanatory prose sentence length | ≤ 22 words (target average ~14) | Count words between full stops |
| Hard ceiling for any sentence | ≤ 35 words | Count words; rewrite if over |
| Parentheticals per sentence | ≤ 1 | Count `(...)` |
| Em-dash clauses per sentence | ≤ 1 | Count `—` and `--` |
| Semicolons per sentence | ≤ 1 | Count `;` |
| Ideas joined with "and" | ≤ 2 | Read it aloud; if it lists three things, use a bullet list |
| Subordinate clauses ("which", "that", "where") | ≤ 1 per sentence | Count |

**Why the limits.** FastAPI averages around 10–14 words per sentence in tutorial prose. Sonda's current docs average 16–19 with flagship sentences at 35–65 words. The target is FastAPI's range. Sentences with multiple parentheticals or em-dashes carry hidden complexity for non-native readers even when the word count is acceptable.

**The 2-second rule.** If a non-native reader would pause for 2+ seconds to translate or interpret a word or phrase, replace it. This catches the long tail of domain-adjacent idioms that are not on the explicit avoid lists in Sections 4 and 6. The lists are not exhaustive; they cover the most frequent offenders. When in doubt, the simpler word wins.

Example: "the distribution centre drifts higher" passes Section 4 (no listed verbs) but fails the 2-second rule. A non-native reader who knows statistics still has to translate "centre drifts" into "mean increases." Rewrite to "the average value increases" or "the mean of the distribution moves higher."

**Rule for lists.** If a sentence has more than two coordinated ideas, replace the sentence with a bullet list. Example:

- **Avoid**: "Sonda generates metrics, logs, traces, and flows, and supports Prometheus, OTLP, Loki, Kafka, syslog, and stdout sinks, with `sine`, `step`, `spike`, `constant`, `sequence`, `sawtooth`, `uniform`, and `csv_replay` generators."
- **Prefer**: "Sonda generates four signal types: metrics, logs, traces, and flows. See [Sinks](...) for the list of supported destinations and [Generators](...) for the list of value patterns."

## 4. Verb-choice rules

The following verbs and idioms read as US-tech slang to international readers. Replace them on sight. The right column shows the plain alternative.

| Avoid | Use instead | Reason |
|---|---|---|
| ship (verb) | send, deliver, release | US business slang |
| ship a binary | release a binary, distribute a binary | same |
| land (in a backend) | arrive, reach | metaphor without anchor |
| pretend to be | model, simulate, represent | playful register for technical prose |
| reach for it when | use it when, choose it when | informal |
| scaffold (verb) | generate, create, write | web-dev slang |
| author (verb) | write, create | overly formal |
| swallow (an alert/spike) | hide, mask, miss | violent metaphor |
| sugar / syntactic sugar | shortcut, equivalent to | programming-language jargon |
| desugar | expand to, equivalent to | same |
| spin up | start, run, launch | informal |
| fire up | start, run | informal |
| wire up | connect, set up | informal |
| out of the box | by default, with no setup | idiom |
| out-of-the-box | built-in, default | same |
| dispatch (to a CLI) | runs, hands off to | jargon |
| under the hood | internally | idiom |
| plumb (a value through) | pass, forward | jargon |
| bake in (a default) | include, set as default | informal |
| roll your own | write your own | informal |
| stand up (a stack) | start, run | informal |
| punt on | defer, skip for now | US sports idiom |
| spin off (a process) | start, launch | informal |
| eat (an error) | ignore, drop | violent metaphor |
| hammer (a service) | send many requests to | violent metaphor |
| spike (verb, "spike a value") | set, send | jargon collision with the generator name |
| swap in | replace with, use instead | informal |
| pull in (a dependency) | add, include | informal |
| crank up (a rate) | increase, raise | informal |
| tee up | prepare, set up | US sports idiom |
| reach into (a config) | open, edit | informal |
| dive into | read, look at | idiom |
| just (as in "just run") | (delete the word) | minimises effort the reader may not feel |

Add new entries as you find them. Treat this table as living.

**Special note on "just."** The word "just" is the single most common pedagogy failure across the docs. "Just run `sonda run`" tells a reader who is stuck that the answer is obvious — but it is not obvious to them. Delete the word unless it is load-bearing time-sense ("just released"). Almost every "just" in instructional prose can be removed without loss.

## 5. Concept presentation rules

### The vocabulary-before-YAML rule

If a YAML field name appears in a code block, every field name in that block must have been defined in prose **on the same page, in the same section or an earlier one, before the code block runs**.

Concretely: a code block like

```yaml
version: 2
kind: runnable
defaults:
  rate: 1
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: cpu
    signal_type: metrics
    generator:
      type: constant
      value: 42
```

is forbidden until prose on the same page has defined `version`, `kind`, `defaults`, `rate`, `encoder`, `sink`, `scenarios`, `id`, `signal_type`, and `generator`. If the prose has not introduced them yet, the code block must show a smaller subset — or the prose must come first.

### Forbidden patterns

- Code blocks with four or more unexplained field names.
- Story or scene-setting in opening paragraphs ("3 a.m., the pager goes off…", "imagine you're a network engineer…"). Open with a one-sentence summary of what this page covers.
- "What next" or "Related" sections with more than four cards.
- Showing the full-featured example first. The first code block on a page must be the minimal working example.

### Required patterns

- Every new concept is introduced in prose first, in ≤ 2 sentences, before any code block shows it.
- Code blocks are ordered: **minimal working example → variant with one new thing → variant with two new things**. Each variant adds exactly one new field or concept.
- Every code block has a `title=` attribute naming the file or labelling the output.
- Every output block is labelled `title="Output"` or similar.

## 6. Idiom blacklist

Catalogue of idioms and metaphors found across the Sonda docs that confuse non-native readers. Treat the third column as a regression flag: entries marked "do not regress" were cleaned in earlier passes and must not return.

| Phrase | Plain alternative | Status |
|---|---|---|
| moving parts | parts, components | do not regress |
| the floor not the ceiling | the minimum, not the maximum | do not regress |
| fit inside each other | nested, nested concepts | do not regress |
| pipe stdout and only data flows through | if you pipe stdout, only the data passes through | do not regress |
| ship it / land in the backend | send / arrive | do not regress |
| the mental model | How Sonda works | do not regress |
| no custom shim per stack | no extra setup needed per stack | do not regress |
| under the hood | internally | active |
| out of the box | by default | active |
| reach for X | use X, choose X | active |
| stand on its own | be complete on its own | active |
| three a.m., the pager goes off | (delete; open with what the page covers) | active |
| pretend to be | model, simulate | active |
| swallow the spike | miss the spike, mask the spike | active |
| your for: clause silently swallowed it | the `for:` duration hid it | active |
| close the loop | finish the test, complete the path | active |
| the rest of this page walks each in order | the next sections cover each part | active |
| stands on its own | is self-contained | active |
| jump straight to | go to | active |
| watch whether the alert fires | check whether the alert fires | active |
| the right metric shape | the right metric values and labels | active |
| nothing else generates the right metric shape | no other tool produces these values | active |
| only shows up in production | only appears in production | active |
| you just told it the wrong thing | the rule was wrong | active |
| the whole picture | the full layout, the structure | active |
| four nouns that fit inside each other | four nested parts | active |
| in passing | briefly | active |
| now you'll learn the names for them | this page names each part | active |
| matches the format real telemetry uses | matches the format of real telemetry | active |
| exercise them | test them | active |
| without needing real production traffic | without using production traffic | active |
| smallest working example | minimal example | active |
| at a glance | quickly, in one look | active |
| good to go | ready | active |
| flap (in prose, not as the generator name) | toggle, alternate | active |
| sane defaults | reasonable defaults, useful defaults | active |
| sensible defaults | default values that work for most cases | active |
| straight out of the box | by default | active |
| zero config | with no configuration | active |
| zero-touch | automatic, no setup | active |
| play nicely with | work with | active |
| first-class | fully supported | active |
| heavy lifting | the main work | active |
| catch a regression | detect a regression | active |
| once and for all | (delete) | active |
| at the end of the day | (delete) | active |
| under the covers | internally | active |
| in flight | in progress, being processed | active |
| from the get-go | from the start | active |
| in the weeds | in detail | active |
| north star | goal, target | active |
| table stakes | required, expected baseline | active |
| low-hanging fruit | easy wins, simple cases | active |
| green-field | new, no existing system | active |
| brown-field | existing system, retrofitted | active |
| dogfood | use internally | active |
| eat your own | use internally | active |

### Domain-adjacent idioms

These read as plain English to a US engineer but force a non-native reader to pause and translate. They are not on the Section 4 verb list because they are not generic slang — they cluster in monitoring, statistics, and electronics prose. Apply the 2-second rule from Section 3.

| Avoid | Use instead | Notes |
|---|---|---|
| pegged at X, pegged | stuck at X, fixed at X, holding at X | informal English |
| ramp (noun for rate of change) | rate of increase, climb, gradient | informal |
| the metric ramps | the metric rises, the metric increases | |
| hysteresis | (define inline first use) or "intentional lag before changing state" | electronics jargon |
| flapping (in a heading) | rapid alternation between firing and resolved | always define inline first use |
| paging fatigue | alert fatigue | "paging" is US-tech-specific |
| drifts higher / drifts lower | rises / falls / increases over time | physics metaphor |
| Past about 30 values | After 30 values, When you have more than 30 values | informal |
| the metric climbs | the metric increases | acceptable in moderation; avoid clustering |
| the metric dips | the metric decreases | acceptable in moderation; avoid clustering |
| spike (verb) | rise sharply | reserve "spike" for the named generator and the `cardinality_spikes` field |

Add new entries as you find them.

## 7. Admonition usage

Material's admonitions keep the main prose clean. Use them for material that is **true and useful but not load-bearing for the reader's main path**.

| Admonition | Use for | Don't use for |
|---|---|---|
| `!!! info` | Background context, optional reading | The main concept |
| `!!! tip` | A better way, an optimisation, a related pattern | A required step |
| `!!! warning` | Footguns, common mistakes, breaking changes | General advice |
| `!!! note` | Side notes, terminology clarifications | The definition itself |
| `!!! example` | A standalone worked example | The page's main example |
| `!!! check` | "You should now have..." verification | Optional check |
| `??? tip` (collapsible) | Advanced details most readers skip | Important content |

Rules:

- Admonitions are short. 1–3 sentences. If an admonition needs a long code block, it probably belongs in the main flow.
- Do not nest admonitions.
- The first line after the admonition header is a single sentence summarising the point. Then optional detail.
- Never use admonitions to repeat content that is already in the main prose.

## 8. Section-structure rules

Every page follows this shape:

1. **Frontmatter** with `title` and `description` (one sentence, ≤ 160 chars, no idioms).
2. **H1** matching the title.
3. **One-sentence opening summary** of what this page covers. No story, no scene-setting.
4. **H2 sections**, each starting with a one-sentence summary of that section.
5. **Numbered steps** (Step 1, Step 2…) for sequential procedures. Plain prose for conceptual pages.
6. **Tables** for any reference content with three or more rows of fields, options, or comparisons.
7. **A closing "What next" or "Related" section** with no more than four links. Each link has a one-sentence reason to follow it.

No H4 or deeper. If you need H4, split the page.

Section length: aim for ≤ 200 words per H2 section. If a section is longer, it is doing two jobs — split it.

## 9. Worked examples

These two rewrites show what "good" looks like under this guide. Read them before you write.

### Example 1: `docs/site/docs/test/alert-testing.md` opening

**Current** (lines 8–12 of the source):

> 3 a.m. The pager goes off for `HighRequestLatency`. By the time you log in, latency is back below threshold and the alert has cleared. You spend an hour reading dashboards and find nothing -- the spike was real, but it lasted 90 seconds and your `for: 5m` clause silently swallowed it. The alert is doing exactly what you told it to. You just told it the wrong thing.
>
> That whole class of problem -- `for:` durations that swallow real spikes, gap-fill rules that fire during scrape outages, compound `A AND B` rules where the two signals never overlap -- only shows up in production because nothing else generates the right metric shape. Sonda does. You write the alert, run a scenario that crosses the threshold for exactly the duration you care about, and watch whether the alert fires.
>
> This page collects the six patterns into one place. Each tab below stands on its own — jump straight to the one that matches the rule you are testing. The table maps common alert shapes to the right tab.

**Rewritten**:

> This page covers six patterns for testing Prometheus alert rules with Sonda. Each pattern targets a class of bug that is hard to reproduce in production: thresholds, short and long `for:` durations, resolution and flapping, compound rules, cardinality, replay of past incidents, and histogram-based alerts.
>
> For each pattern, Sonda generates a metric stream with the exact shape your rule expects. You define the alert, run the scenario, and check whether the alert fires.
>
> Use the table below to find the pattern that matches your rule. Each section is self-contained — you can read only the one you need.

**Annotations**:

- Sentence count: 5 → 5 (similar). Word count: 158 → 95.
- Average words per sentence: 31.6 → 19. Maximum sentence: 53 words → 28 words.
- Removed idioms: "3 a.m. The pager goes off," "silently swallowed," "you just told it the wrong thing," "only shows up in production," "the right metric shape," "stands on its own," "jump straight to," "collects... into one place."
- Opens with a one-sentence summary of what the page covers (Section 8 rule 3). The story is gone.
- The list of bug classes is now a colon-separated list inside one sentence instead of a 53-word sentence with three em-dash clauses.
- "Each pattern targets a class of bug" replaces "that whole class of problem... only shows up in production because nothing else generates the right metric shape" — the same information without the metaphor.
- "Check whether the alert fires" replaces "watch whether the alert fires." Plain verb.

### Example 2: `docs/site/docs/get-started/your-first-scenario.md` opening

**Current** (lines 8–14 of the source):

> ## The mental model
>
> Sonda is a synthetic telemetry generator. You write a YAML recipe that says *"pretend to be a CPU metric that oscillates between 40% and 80%"* or *"pretend to be a router emitting interface counters"* or *"pretend to be an application emitting JSON logs at 100/sec"* — Sonda produces realistic-looking data that matches the format real telemetry uses and ships it to your sinks (stdout, a file, Prometheus remote-write, Loki, Kafka, OTLP). You point your dashboards, alert rules, and ingestion pipelines at the synthetic stream and exercise them without needing real production traffic.
>
> The whole picture is four nouns that fit inside each other: a **scenario file** declares what to emit; inside it, one or more entries each pick a **generator** (the value pattern), an **encoder** (the wire format), and a **sink** (the destination). The rest of this page walks each in order, with the smallest working example for each.
>
> You've already seen the four parts in passing if you ran through the [Quickstart](quickstart.md). Now you'll learn the names for them and what each one lets you change.

**Rewritten**:

> ## How Sonda works
>
> A Sonda **scenario** is a YAML file that describes the telemetry you want to generate. For example: a CPU metric that oscillates between 40% and 80%, a router emitting interface counters, or an application emitting JSON logs at 100 messages per second. Sonda reads the file and sends realistic data to the destinations you choose: stdout, a file, Prometheus remote-write, Loki, Kafka, or OTLP.
>
> You point your dashboards, alert rules, and ingestion pipelines at this synthetic data and test them without production traffic.
>
> A scenario has four parts:
>
> - A **scenario file** — the YAML document `sonda run` reads.
> - One or more **generators** — each one produces a value pattern.
> - An **encoder** — converts values into the wire format (Prometheus text, JSON Lines, OTLP, and so on).
> - A **sink** — the destination that receives the encoded data.
>
> The next sections cover each part, starting with the minimal example.

**Annotations**:

- Sentence count: 5 → 8 sentences plus a 4-item bullet list. Word count: 196 → 127.
- Average sentence length: 39 → 14 words. Maximum sentence: 81 words → 30 words.
- The opening 81-word sentence is split into three sentences. The three "pretend to be…" examples become a single colon-list. No em-dash clauses.
- Removed idioms: "the mental model" → "How Sonda works"; "pretend to be" → "describes" / "for example"; "moving parts" was implicit and gone; "ships it" → "sends"; "fit inside each other" → bullet list; "the whole picture" → "A scenario has four parts"; "exercise them" → "test them"; "smallest working example" → "minimal example"; "in passing" deleted; "Now you'll learn the names for them" deleted.
- The four parts are now a bullet list instead of a 50-word sentence with three em-dashed parentheticals.
- The forward-reference "the next sections cover each part" replaces "the rest of this page walks each in order."
- Each part is introduced in prose before any code block — satisfies Section 5.

## 10. Self-check checklist

Before reporting any docs work, run this list on every paragraph you wrote or edited.

1. Is the opening sentence of the page a single-sentence summary of what the page covers — not a story, not scene-setting?
2. Are all sentences ≤ 22 words? Any sentence > 35 words?
3. Does any sentence contain more than one parenthetical, em-dash clause, or semicolon?
4. Does any sentence have more than two ideas joined with "and"?
5. Are any verbs or phrases from Section 4 (verb-choice rules) present?
6. Are any idioms from Section 6 (idiom blacklist) present? In particular, anything marked "do not regress"?
7. Is every YAML field name in every code block defined in prose earlier on the same page?
8. Is the first code block on each page the minimal working example, not the full-featured one?
9. Does every code block have a `title=` attribute?
10. Does any H2 section exceed ~200 words, or contain two distinct concepts that should be split?
11. Are tangents in admonitions, not in main-sentence em-dashes?
12. Does the "What next" or "Related" section have four or fewer links?
13. Have you deleted every "just" that does not carry time-sense?
14. Read the paragraph aloud — does it sound like FastAPI's tutorial, or like a US engineer chatting with another US engineer?
15. Did you apply the 2-second rule? Read each sentence aloud. If any word makes you pause to translate, replace it.

If any answer is wrong, fix it before reporting work complete.

## Reference

- FastAPI tutorial — the pedagogy benchmark: [https://fastapi.tiangolo.com/tutorial/first-steps/](https://fastapi.tiangolo.com/tutorial/first-steps/)
- Material for MkDocs admonitions reference: [https://squidfunk.github.io/mkdocs-material/reference/admonitions/](https://squidfunk.github.io/mkdocs-material/reference/admonitions/)
