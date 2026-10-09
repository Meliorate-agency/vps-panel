/* Sidebar nav glyphs.
 *
 * Hand-drawn rather than pulled from an icon package: the panel ships no icon
 * dependency and this set is twenty marks, not two thousand.
 *
 * House rules for anything added here:
 *   - 24x24 viewBox, artwork inside 2.5..21.5, whole/half-pixel coordinates.
 *   - Outline geometry only. stroke, width, caps and joins come from <NavIcon>
 *     below, so the glyphs themselves carry no stroke attributes. The one
 *     exception is a solid indicator dot, which sets fill="currentColor".
 *   - currentColor throughout, never a literal colour: Sidebar.module.css
 *     recolours the icon on hover and for the active row, and the whole panel
 *     is re-themed from a single operator-chosen accent.
 *   - Silhouettes must stay distinct. These render 16px tall, stacked ~6px
 *     apart in one column, so "a rounded rect with something in it" is a
 *     shape the set can afford maybe twice.
 */

const GLYPHS = {
  // Bento panels, deliberately uneven - an even 2x2 grid reads as "apps".
  dashboard: (
    <>
      <rect x="3.5" y="3.5" width="6.5" height="8.5" rx="1.5" />
      <rect x="14" y="3.5" width="6.5" height="5" rx="1.5" />
      <rect x="14" y="12" width="6.5" height="8.5" rx="1.5" />
      <rect x="3.5" y="15.5" width="6.5" height="5" rx="1.5" />
    </>
  ),
  // Isometric crate: a deployed app is a thing you ship.
  apps: (
    <>
      <path d="M12 3L20.5 8V16L12 21L3.5 16V8Z" />
      <path d="M3.5 8L12 13L20.5 8" />
      <path d="M12 13V21" />
    </>
  ),
  // Clock face on an open arc with an arrowhead: time, but recurring.
  cron: (
    <>
      <path d="M20 12A8 8 0 1 1 18.4 7.2" />
      <path d="M18.4 4.6L18.4 7.2L16 6.4" />
      <path d="M12 7.5V12L15.5 13.5" />
    </>
  ),
  // Cog. The only gear in the set - Settings uses sliders precisely so this
  // one stays unambiguous. Teeth are radial stubs off a ring rather than a
  // toothed outline; a true toothed profile turns to mush at 16px.
  services: (
    <>
      <circle cx="12" cy="12" r="6.5" />
      <circle cx="12" cy="12" r="2.5" />
      <path d="M12 5.5V3M12 18.5V21M5.5 12H3M18.5 12H21M7.4 7.4L5.6 5.6M16.6 16.6L18.4 18.4M16.6 7.4L18.4 5.6M7.4 16.6L5.6 18.4" />
    </>
  ),
  // Drive enclosure: platter seam across the middle, two activity LEDs on the
  // lower band. Resist adding a long slot line under the seam - a rounded rect
  // with one full-width rule and one short one below it reads as a bank card.
  drive: (
    <>
      <rect x="3" y="5" width="18" height="14" rx="2" />
      <line x1="3" y1="12" x2="21" y2="12" />
      <circle cx="6.5" cy="15.5" r="1" fill="currentColor" />
      <circle cx="10" cy="15.5" r="1" fill="currentColor" />
    </>
  ),
  files: <path d="M3 17V7.5a2 2 0 0 1 2-2h4l2.5 3H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />,
  domains: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <ellipse cx="12" cy="12" rx="3.5" ry="8.5" />
      <line x1="3.5" y1="12" x2="20.5" y2="12" />
    </>
  ),
  // Bent arrow: a request comes in along the top and is sent on somewhere
  // else. The only arrow in the set, so it cannot be confused with the
  // globe above it.
  redirects: (
    <>
      <path d="M3.5 7H13.5a4 4 0 0 1 4 4v7" />
      <path d="M14 14.5L17.5 18L21 14.5" />
    </>
  ),
  // Two servers with a pulse running between them: they check on each
  // other. The only zigzag in the set; MCP's hub has no line between peers.
  networks: (
    <>
      <circle cx="5" cy="12" r="2.5" />
      <circle cx="19" cy="12" r="2.5" />
      <path d="M7.5 12H9.5L11 8.5L13 15.5L14.5 12H16.5" />
    </>
  ),
  mailboxes: (
    <>
      <rect x="3" y="5.5" width="18" height="13" rx="2" />
      <path d="M3 8L12 13L21 8" />
    </>
  ),
  databases: (
    <>
      <ellipse cx="12" cy="6" rx="8" ry="3" />
      <path d="M4 6v12a8 3 0 0 0 16 0V6" />
      <path d="M4 12a8 3 0 0 0 16 0" />
    </>
  ),
  // Archive crate, kept deliberately flat so it never reads as the database
  // drum sitting two rows above it.
  backups: (
    <>
      <rect x="3" y="3.5" width="18" height="4.5" rx="1.5" />
      <path d="M5 8v11a1.5 1.5 0 0 0 1.5 1.5h11a1.5 1.5 0 0 0 1.5-1.5V8" />
      <path d="M9.5 14h5" />
    </>
  ),
  terminal: (
    <>
      <rect x="2.5" y="4" width="19" height="16" rx="2" />
      <path d="M6.5 9L10 12L6.5 15" />
      <path d="M12.5 15h5" />
    </>
  ),
  // Frameless log stream: timestamp stubs paired with ragged output. A page
  // outline here would collide with Audit, which is a page plus a magnifier.
  logs: (
    <>
      <path d="M3 4.5h3M3 9.5h3M3 14.5h3M3 19.5h3" />
      <path d="M8.5 4.5h12.5M8.5 9.5h9M8.5 14.5h12M8.5 19.5h7.5" />
    </>
  ),
  tasks: (
    <>
      <path d="M3 7l2.5 2.5L10 4.5" />
      <path d="M13 7H21" />
      <path d="M3 13H21" />
      <path d="M3 19H21" />
    </>
  ),
  // The record, plus a lens over it. The lens tucks over the page's corner
  // rather than crossing its body - overlapping outlines go to mush at 16px.
  audit: (
    <>
      <rect x="3.5" y="3" width="11" height="12" rx="2" />
      <path d="M6.5 7H11.5" />
      <path d="M6.5 10.5H9.5" />
      <circle cx="16.5" cy="16.5" r="3.5" />
      <path d="M19 19L20.5 20.5" />
    </>
  ),
  // Brick courses with staggered joints. Not a shield: a shield says "we are
  // protected", a wall says "here is the barrier you are editing".
  firewall: (
    <>
      <rect x="3" y="3.5" width="18" height="16.5" rx="2" />
      <path d="M3 9H21M3 14.5H21" />
      <path d="M8 3.5V9M16 3.5V9M12 9V14.5M8 14.5V20M16 14.5V20" />
    </>
  ),
  users: (
    <>
      <circle cx="9" cy="7" r="3" />
      <path d="M3 20v-2a4.5 4.5 0 0 1 4.5-4.5h3a4.5 4.5 0 0 1 4.5 4.5v2" />
      <path d="M16 4a3 3 0 0 1 0 6" />
      <path d="M16 13.5a4.5 4.5 0 0 1 4.5 4.5v2" />
    </>
  ),
  sshKeys: (
    <>
      <circle cx="16.5" cy="7.5" r="4" />
      <path d="M13.5 10.5L3.5 20.5" />
      <path d="M8 16L6 14M5.5 18.5L3.5 16.5" />
    </>
  ),
  notifications: (
    <>
      <path d="M6.5 13.5V8.5a5.5 5.5 0 0 1 11 0v5l3 3H3.5L6.5 13.5Z" />
      <path d="M9.5 19a3 3 0 0 0 5 0" />
    </>
  ),
  // Sliders, not a cog - the cog belongs to System services.
  settings: (
    <>
      <path d="M3.5 6H6M10 6H20.5M3.5 12H13.5M17.5 12H20.5M3.5 18H9M13 18H20.5" />
      <circle cx="8" cy="6" r="2" />
      <circle cx="15.5" cy="12" r="2" />
      <circle cx="11" cy="18" r="2" />
    </>
  ),
  // Hub and spokes: an agent endpoint with tools hanging off it.
  mcp: (
    <>
      <circle cx="12" cy="12" r="2.5" />
      <circle cx="5.5" cy="5.5" r="2" />
      <circle cx="18.5" cy="5.5" r="2" />
      <circle cx="12" cy="19.5" r="2" />
      <path d="M10.2 10.2L7 7M13.8 10.2L17 7M12 14.5V17.5" />
    </>
  ),
} as const;

export type NavIconName = keyof typeof GLYPHS;

export default function NavIcon({ name, className }: { name: NavIconName; className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {GLYPHS[name]}
    </svg>
  );
}
