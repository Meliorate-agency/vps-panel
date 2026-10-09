"use client";

import { useMemo, useState } from "react";
import Table from "@/components/Table/Table";
import local from "./mailboxes.module.css";

/**
 * One mailbox row, pre-rendered on the server. `cells` holds the finished
 * React nodes (including the bound server actions for edit/delete), so this
 * client component only decides which rows to show — it never re-derives
 * mailbox data.
 */
export type MailboxRow = {
  id: number;
  domain: string;
  cells: Record<string, React.ReactNode>;
};

const COLUMNS = [
  { key: "email", label: "Email" },
  { key: "quota", label: "Quota", width: "200px" },
  { key: "copy", label: "", width: "110px" },
  { key: "actions", label: "", width: "200px" },
];

export default function MailboxTable({ rows }: { rows: MailboxRow[] }) {
  const [selected, setSelected] = useState("all");

  const domains = useMemo(() => {
    const counts = new Map<string, number>();
    for (const r of rows) counts.set(r.domain, (counts.get(r.domain) ?? 0) + 1);
    return [...counts.entries()]
      .map(([name, count]) => ({ name, count }))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [rows]);

  // Deleting the last mailbox of a domain drops that domain from the chip
  // list. Derive the active filter instead of storing it so we fall back to
  // "all" rather than stranding the user on an empty table with no chip to
  // click back to.
  const active =
    selected !== "all" && domains.some((d) => d.name === selected) ? selected : "all";

  const visible = active === "all" ? rows : rows.filter((r) => r.domain === active);

  return (
    <>
      {domains.length > 1 && (
        <div
          className={local.domainFilter}
          role="group"
          aria-label="Filter mailboxes by domain"
        >
          <Chip
            label="All domains"
            count={rows.length}
            active={active === "all"}
            onClick={() => setSelected("all")}
          />
          {domains.map((d) => (
            <Chip
              key={d.name}
              label={d.name}
              count={d.count}
              active={active === d.name}
              onClick={() => setSelected(d.name)}
            />
          ))}
        </div>
      )}

      <div className={local.mailboxTable}>
        <Table
          columns={COLUMNS}
          rowKey={(_row, i) => visible[i].id}
          rows={visible.map((r) => r.cells)}
          empty="No mailboxes yet."
        />
      </div>
    </>
  );
}

function Chip({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={[local.domainChip, active ? local.domainChipActive : ""]
        .join(" ")
        .trim()}
    >
      {label}
      <span className={local.domainChipCount}>{count}</span>
    </button>
  );
}
