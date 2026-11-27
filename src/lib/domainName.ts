/**
 * Domain name helpers shared by the Add domain form and its server action
 * (no server-only imports: the form is a client component).
 */

/** Second-level labels under a country code that are sold as one name:
 *  example.co.uk, example.com.au, example.com.hr. Not the whole public suffix
 *  list, the shapes that are actually registered. */
const SECOND_LEVEL = new Set([
  "ac", "biz", "co", "com", "edu", "gen", "go", "gob", "gov", "info", "int", "ltd", "me",
  "mil", "ne", "net", "nhs", "nom", "or", "org", "plc", "sch",
]);

/** How many labels the registrable name has: 3 for example.co.uk, else 2. */
function registrableLabels(labels: string[]): number {
  const n = labels.length;
  if (n >= 3 && labels[n - 1].length === 2 && SECOND_LEVEL.has(labels[n - 2])) return 3;
  return 2;
}

/**
 * A name below a registrable domain (shop.example.com, shop.example.co.uk).
 * Counting labels alone made example.co.uk a subdomain, so Add domain would
 * not set up its mail or its www redirect.
 */
export function isSubdomainName(domain: string): boolean {
  const labels = domain
    .toLowerCase()
    .split(".")
    .filter((l) => l.length > 0);
  return labels.length > registrableLabels(labels);
}
