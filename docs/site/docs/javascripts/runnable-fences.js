/* Sonda docs — "Run in playground" buttons on complete scenario fences.
 *
 * Progressive enhancement, site-wide: every YAML code fence that is a
 * complete scenario gets a link that opens it in the playground, carried in
 * the existing `#yaml=` hash. A reader who wants to see what a documented
 * scenario actually does no longer has to copy, switch pages, and paste.
 *
 * Three properties are load-bearing:
 *
 * 1. The decision of what counts as runnable is NOT made here — it lives in
 *    `runnableScenario` (sonda-pure.js) and is answered by the same case
 *    table as the CI gate that compiles every buttoned fence
 *    (scripts/validate_docs_scenarios.py). A button therefore cannot promise
 *    something CI has not proven.
 * 2. Nothing is ever built by assigning markup — createElement/textContent
 *    only. Fence text is repo-authored and PR-reviewed, but the rule is
 *    absolute (see the CI grep gate in the docs-commands job) precisely so it
 *    never has to be re-argued per call site.
 * 3. Failure is silent and total. If the playground cannot be located, no
 *    buttons appear and the page is untouched — the fences are still
 *    readable prose, which is the whole no-JS floor.
 */
import { runnableScenario, toBase64Url } from "./sonda-pure.js";
import { playgroundHref } from "./playground-link.js";

const LINK_CLASS = "sonda-runnable";

function buildLink(href, yaml) {
  const link = document.createElement("a");
  link.className = LINK_CLASS;
  link.href = `${href}#yaml=${toBase64Url(yaml)}`;
  link.textContent = "Run in playground →";
  // The fence right above says which scenario this is; screen readers reading
  // the link out of context get the association spelled out.
  link.setAttribute("aria-label", "Run this scenario in the Sonda playground");
  return link;
}

function boot() {
  // The playground page's own fences must not offer a link back to the page
  // the reader is already on.
  if (document.getElementById("sonda-playground")) return;

  const content = document.querySelector(".md-content");
  if (!content) return;

  const href = playgroundHref();
  if (!href) return; // nav missing or renamed — leave the page alone

  for (const code of content.querySelectorAll("pre > code")) {
    // `.highlight` is the wrapper pymdownx puts around the whole block
    // (including a `title=` filename bar, when present); the link belongs
    // after it, not between the title and its code.
    const block = code.closest(".highlight") || code.parentElement;
    if (!block || block.dataset.sondaRunnable) continue;

    // Material tags the language on the wrapper (pygments_lang_class). When
    // the fence carries no language at all, fall through to the detector on
    // the text itself rather than skipping — an unclassed complete scenario
    // is still a complete scenario.
    const classed = block.className || "";
    if (classed.includes("language-") && !classed.includes("language-yaml")) continue;

    const yaml = code.textContent;
    if (!runnableScenario(yaml)) continue;

    block.dataset.sondaRunnable = "1";
    block.insertAdjacentElement("afterend", buildLink(href, yaml));
  }
}

/* Material's instant navigation swaps `.md-content` without a page load, so
 * boot() re-runs per document. The `data-sonda-runnable` flag lives on the
 * swapped-in DOM, so a re-run over the same document is a no-op rather than a
 * source of duplicate links. */
if (window.document$ && typeof window.document$.subscribe === "function") {
  window.document$.subscribe(boot);
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
