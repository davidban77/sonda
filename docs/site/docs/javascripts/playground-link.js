/* Sonda docs — locate the playground from any page in the site.
 *
 * Two enhancements need this: the "Run in playground →" buttons on runnable
 * fences (runnable-fences.js) and the "Open in playground →" links on the
 * examples gallery (livegen.js). They ran at different depths of the tree and
 * would otherwise each carry their own answer to the same question.
 *
 * The site is published under a project prefix (/sonda/), and these links
 * appear at every depth, so neither a root-absolute path nor a fixed number
 * of `../` hops is correct everywhere. The nav is rendered on every page and
 * its hrefs are already resolved relative to the current one; reading `.href`
 * yields the absolute URL.
 *
 * The `$=` match pins the playground index and excludes `playground/alert-lab/`,
 * which a `*=` match would also hit — and which, being second in the nav,
 * would otherwise win on some pages but not others.
 *
 * Returns null rather than a guess when the nav is missing or renamed. Every
 * caller treats that as "add no link", which leaves the page exactly as the
 * markdown rendered it — the no-JS floor.
 */
export function playgroundHref() {
  const exact = document.querySelector('.md-nav a[href$="playground/"]');
  if (exact) return exact.href;
  const loose = document.querySelector('.md-nav a[href*="playground/"]');
  return loose ? loose.href : null;
}
