import styles from "./Table.module.css";

type Row = Record<string, React.ReactNode>;

type Props = {
  columns: { key: string; label: string; width?: string }[];
  rows: Array<Row>;
  /**
   * Optional stable key per row. Pass this for tables whose rows contain
   * inline editors or popovers (EditMailboxRow, EditRecordRow): without a
   * stable key React reuses row DOM by index when rows are reordered or
   * filtered, snapping open editors onto the wrong row.
   */
  rowKey?: (row: Row, index: number) => string | number;
  empty?: string;
  /**
   * Set to "fixed" when one column may contain unbounded text (e.g. DNS TXT
   * records, log lines). Default `auto` lets the browser grow columns to fit
   * content, which can push neighbours off the side of the viewport.
   */
  layout?: "auto" | "fixed";
  /**
   * Enable mobile-only horizontal scroll inside the purple card itself
   * (the `.wrap` div) instead of letting the table push past the viewport.
   * Only safe on tables whose cells contain NO popovers with `position:
   * absolute` — the `.wrap` becomes a clipping/stacking context and
   * traps absolutely-positioned descendants. For tables that DO have
   * popovers (apps, mailboxes), set this true AND portal the popover to
   * document.body to escape the clip.
   */
  scrollX?: boolean | "always";
  /**
   * Show at most this many rows and scroll inside the card for the rest,
   * with the header pinned (long lists like the firewall's open ports).
   * Rows differ in height, so this is an estimate: tall rows show fewer.
   */
  maxRows?: number;
};

/** One row and the header, in px, for the maxRows estimate. */
const ROW_PX = 44;
const HEAD_PX = 37;
/** Room for a column that declares no width, in the scrollX="always" sum. */
const COL_PX = 120;

/** The width a table needs before its columns start squeezing: without it
 *  `width: 100%` just shrinks every column to fit the card and nothing
 *  scrolls. */
function minTableWidth(columns: Props["columns"]): number {
  return columns.reduce((sum, c) => {
    const px = c.width?.endsWith("px") ? Number.parseFloat(c.width) : NaN;
    return sum + (Number.isFinite(px) ? px : COL_PX);
  }, 0);
}

export default function Table({
  columns,
  rows,
  rowKey,
  empty = "Nothing here yet.",
  layout = "auto",
  scrollX = false,
  maxRows,
}: Props) {
  if (rows.length === 0) {
    return <div className={styles.empty}>{empty}</div>;
  }
  const capped = maxRows !== undefined && rows.length > maxRows;
  return (
    <div
      className={[
        styles.wrap,
        scrollX === "always" ? styles.wrapScrollXAlways : scrollX ? styles.wrapScrollX : "",
        capped ? styles.wrapScrollY : "",
      ]
        .join(" ")
        .trim()}
      style={capped ? { maxHeight: HEAD_PX + ROW_PX * maxRows } : undefined}
    >
      <table
        className={styles.table}
        style={{
          ...(layout === "fixed" ? { tableLayout: "fixed" as const } : null),
          ...(scrollX === "always" ? { minWidth: minTableWidth(columns) } : null),
        }}
      >
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} style={c.width ? { width: c.width } : undefined}>
                {c.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={rowKey ? rowKey(row, i) : i}>
              {columns.map((c) => (
                <td key={c.key}>{row[c.key] ?? <span className={styles.dim}>-</span>}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
