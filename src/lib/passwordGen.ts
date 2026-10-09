/**
 * Client-side password generator for the "Generate" buttons next to
 * password fields. Plain module (no server-only) so client components can
 * import it; uses the Web Crypto API, never Math.random.
 *
 * 20 characters from letters, digits and a small set of symbols that are
 * safe to paste into any mail client or shell without quoting. Rejection
 * sampling keeps the distribution uniform, and the result always contains
 * at least one lowercase, one uppercase, one digit and one symbol.
 */
const LOWER = "abcdefghijkmnopqrstuvwxyz";
const UPPER = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGITS = "23456789";
const SYMBOLS = "-_.!@#%+=";
const ALL = LOWER + UPPER + DIGITS + SYMBOLS;

function pick(alphabet: string, bytes: Uint8Array, cursor: { i: number }): string {
  // Reject values that would bias the modulo so every character is equally likely.
  const limit = 256 - (256 % alphabet.length);
  for (;;) {
    if (cursor.i >= bytes.length) {
      crypto.getRandomValues(bytes);
      cursor.i = 0;
    }
    const b = bytes[cursor.i++];
    if (b < limit) return alphabet[b % alphabet.length];
  }
}

export function generatePassword(length = 20): string {
  const bytes = new Uint8Array(64);
  crypto.getRandomValues(bytes);
  const cursor = { i: 0 };
  const chars: string[] = [
    pick(LOWER, bytes, cursor),
    pick(UPPER, bytes, cursor),
    pick(DIGITS, bytes, cursor),
    pick(SYMBOLS, bytes, cursor),
  ];
  while (chars.length < length) chars.push(pick(ALL, bytes, cursor));
  // Fisher-Yates so the guaranteed classes are not always in front.
  for (let i = chars.length - 1; i > 0; i--) {
    const j = pick(ALL, bytes, cursor).charCodeAt(0) % (i + 1);
    [chars[i], chars[j]] = [chars[j], chars[i]];
  }
  return chars.join("");
}
