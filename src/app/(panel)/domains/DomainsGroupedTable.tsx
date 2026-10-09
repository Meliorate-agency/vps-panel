"use client";

import { useState, type ReactNode } from "react";
import Table from "@/components/Table/Table";
import styles from "./DomainsGroupedTable.module.css";

export type GroupedRow = {
  key: string;
  apex: string;
  isApex: boolean;
  hasChildren: boolean;
  cells: Record<string, ReactNode>;
};

type Props = {
  columns: { key: string; label: string; width?: string }[];
  rows: GroupedRow[];
  empty?: string;
  /** Forwarded to <Table>. When true, the table scrolls horizontally
   *  inside its own purple card on mobile instead of pushing the page
   *  layout. Domains rows have no popovers in cells — safe to enable. */
  scrollX?: boolean;
};

export default function DomainsGroupedTable({ columns, rows, empty, scrollX = false }: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  function toggle(apex: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(apex)) next.delete(apex);
      else next.add(apex);
      return next;
    });
  }

  const visibleRows = rows.filter((r) => r.isApex || !collapsed.has(r.apex));

  const tableRows = visibleRows.map((r) => {
    const cells = { ...r.cells };
    const original = cells.domain;
    if (r.isApex && r.hasChildren) {
      const isOpen = !collapsed.has(r.apex);
      cells.domain = (
        <span className={styles.apexCell}>
          <button
            type="button"
            className={styles.chevron}
            onClick={() => toggle(r.apex)}
            aria-label={isOpen ? "Collapse" : "Expand"}
            aria-expanded={isOpen}
          >
            <span className={isOpen ? styles.chevronOpen : styles.chevronClosed}>
              ▸
            </span>
          </button>
          {original}
        </span>
      );
    } else if (!r.isApex) {
      cells.domain = (
        <span className={styles.childCell}>
          <span className={styles.connector}>└</span>
          {original}
        </span>
      );
    } else {
      cells.domain = <span className={styles.apexFlat}>{original}</span>;
    }
    return cells;
  });

  // Keyed by domain, not position: collapsing a group shifts the rows below,
  // and index keys handed one row's button state (an open confirm, a running
  // Renew) to whichever domain moved into its slot.
  return (
    <Table
      scrollX={scrollX}
      columns={columns}
      rows={tableRows}
      rowKey={(_, i) => visibleRows[i].key}
      empty={empty}
    />
  );
}
