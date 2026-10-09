"use client";

import { useMemo, useState } from "react";
import Table from "@/components/Table/Table";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import ConfirmSubmitButton from "@/components/ConfirmSubmitButton/ConfirmSubmitButton";
import styles from "@/components/page.module.css";
import { serviceActionExec } from "@/server-actions/services";

type Service = {
  unit: string;
  active: string;
  sub: string;
  description: string;
};

const HIGHLIGHT = [
  "nginx.service",
  "mariadb.service",
  "postfix.service",
  "dovecot.service",
  "rspamd.service",
  "pdns.service",
  "fail2ban.service",
  "ufw.service",
  "vps-panel.service",
  "vps-panel-agent.service",
];

export default function ServicesView({ services }: { services: Service[] }) {
  const [query, setQuery] = useState("");

  const { highlighted, others, totalMatches } = useMemo(() => {
    const q = query.trim().toLowerCase();
    const matches = q
      ? services.filter(
          (s) =>
            s.unit.toLowerCase().includes(q) ||
            s.description.toLowerCase().includes(q),
        )
      : services;
    return {
      highlighted: matches.filter((s) => HIGHLIGHT.includes(s.unit)),
      others: matches.filter((s) => !HIGHLIGHT.includes(s.unit)),
      totalMatches: matches.length,
    };
  }, [query, services]);

  function row(s: Service) {
    return {
      unit: <span className={styles.code}>{s.unit}</span>,
      status: <StatusBadge text={s.sub} />,
      description: <span className={styles.muted}>{s.description}</span>,
      actions: (
        <div style={{ display: "flex", gap: 4 }}>
          <form action={serviceActionExec.bind(null, s.unit, "restart")}>
            <ConfirmSubmitButton
              className={styles.btnGhost}
              message={`Restart ${s.unit}?\n\nBriefly interrupts whatever this service is handling.`}
              pendingLabel="..."
            >
              Restart
            </ConfirmSubmitButton>
          </form>
          <form action={serviceActionExec.bind(null, s.unit, "stop")}>
            <ConfirmSubmitButton
              className={styles.btnGhost}
              message={`Stop ${s.unit}?\n\nStopping critical services (nginx, vps-panel, vps-panel-agent, mariadb) will take the panel offline.`}
              pendingLabel="..."
            >
              Stop
            </ConfirmSubmitButton>
          </form>
        </div>
      ),
    };
  }

  const q = query.trim();

  return (
    <>
      <div
        className={styles.section}
        style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}
      >
        <input
          type="search"
          className={styles.input}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter by unit name or description..."
          aria-label="Filter services"
          style={{ flex: 1, minWidth: 240 }}
        />
        {q && (
          <>
            <span className={styles.muted} style={{ fontSize: 12 }}>
              {totalMatches} match{totalMatches === 1 ? "" : "es"}
            </span>
            <button
              type="button"
              className={styles.btnGhost}
              onClick={() => setQuery("")}
            >
              Clear
            </button>
          </>
        )}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Core stack</h2>
        <Table
          scrollX
          columns={[
            // Explicit widths on every column so the table has a real
            // intrinsic width (~860px) and triggers horizontal scroll
            // inside Table.wrap on mobile. Without these, unit/description
            // collapse to fit the viewport and the action buttons get
            // squeezed past readability.
            { key: "unit", label: "Unit", width: "260px" },
            { key: "status", label: "Status", width: "120px" },
            { key: "description", label: "Description", width: "300px" },
            { key: "actions", label: "", width: "180px" },
          ]}
          rowKey={(_r, i) => highlighted[i].unit}
          rows={highlighted.map(row)}
          empty={q ? "No core services match the filter." : "None of the core services are present yet."}
        />
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>All services</h2>
        <Table
          scrollX
          columns={[
            // Explicit widths on every column so the table has a real
            // intrinsic width (~860px) and triggers horizontal scroll
            // inside Table.wrap on mobile. Without these, unit/description
            // collapse to fit the viewport and the action buttons get
            // squeezed past readability.
            { key: "unit", label: "Unit", width: "260px" },
            { key: "status", label: "Status", width: "120px" },
            { key: "description", label: "Description", width: "300px" },
            { key: "actions", label: "", width: "180px" },
          ]}
          rowKey={(_r, i) => others[i].unit}
          rows={others.map(row)}
          empty={q ? "No other services match the filter." : "No services."}
        />
      </div>
    </>
  );
}
