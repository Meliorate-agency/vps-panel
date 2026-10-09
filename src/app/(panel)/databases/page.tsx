import { agent } from "@/lib/agent";
import Table from "@/components/Table/Table";
import QuickInstallStrip from "@/components/QuickInstallStrip/QuickInstallStrip";
import ServiceIcon from "@/components/ServiceIcon/ServiceIcon";
import StatusBadge from "@/components/StatusBadge/StatusBadge";
import { getServerAddress } from "@/lib/serverAddress";
import { uploadLimitMb } from "@/lib/uploadLimit";
import styles from "@/components/page.module.css";
import {
  createDatabaseAction,
  createDbUserAction,
  deleteDatabaseAction,
  deleteDbUserAction,
  grantAction,
  importSqlDumpAction,
  prepareSqlImportAction,
  revokeDbGrantAction,
  setDbUserPasswordAction,
} from "@/server-actions/databases";
import {
  quickInstallAction,
  setBindAction,
  getRoundcubeSettingsAction,
  updateRoundcubeSettingsAction,
  getN8nSettingsAction,
  updateN8nSettingsAction,
  getPhpMyAdminSettingsAction,
  updatePhpMyAdminSettingsAction,
} from "@/server-actions/quickInstall";
import {
  createPgDatabaseAction,
  createPgRoleAction,
  deletePgDatabaseAction,
  deletePgRoleAction,
  disablePgvectorAction,
  enablePgvectorAction,
  pgGrantAction,
  setPgRolePasswordAction,
  updatePgDatabaseAction,
} from "@/server-actions/postgres";
import CreateDbForm from "./CreateDbForm";
import CreateUserForm from "./CreateUserForm";
import GrantForm from "./GrantForm";
import ImportSqlForm from "./ImportSqlForm";
import GrantedUsers from "./GrantedUsers";
import CreatePgDbForm from "./CreatePgDbForm";
import CreatePgUserForm from "./CreatePgUserForm";
import PgGrantForm from "./PgGrantForm";
import PgDbEditButton from "./PgDbEditButton";
import PgVectorButton from "./PgVectorButton";
import ChangePasswordButton from "./ChangePasswordButton";
import ConfirmDeleteButton from "@/components/ConfirmDeleteButton/ConfirmDeleteButton";

export const dynamic = "force-dynamic";

const DB_GUI_SLUGS = [
  "mariadb",
  "phpmyadmin",
  "postgresql",
  "pgadmin",
  "pgvector",
] as const;

export default async function DatabasesPage() {
  const [users, dbs, recipes, addr] = await Promise.all([
    agent.databases.listUsers().catch(() => []),
    agent.databases.listDatabases().catch(() => []),
    agent.quickInstall.list().catch(() => []),
    getServerAddress(),
  ]);
  const serverAddress = addr.domain ?? addr.ip ?? null;
  const mariadb = recipes.find((r) => r.slug === "mariadb");
  const postgres = recipes.find((r) => r.slug === "postgresql");
  const pgadmin = recipes.find((r) => r.slug === "pgadmin");
  const dbRecipes = recipes.filter(
    (r) =>
      (DB_GUI_SLUGS as readonly string[]).includes(r.slug) &&
      // pgvector is a Postgres add-on — only surface its card once Postgres
      // itself is installed; before that the install would just fail.
      (r.slug !== "pgvector" || (postgres?.installed ?? false)),
  );

  // Only fetch Postgres state when the engine is actually installed.
  // Otherwise the agent's psql shell-out would 500 every page render and
  // pollute the audit log with spurious errors. Both endpoints fall back
  // to [] on failure so a transient psql hiccup degrades gracefully
  // instead of breaking the whole page.
  const [pgRoles, pgDbs, pgvector] = postgres?.installed
    ? await Promise.all([
        agent.postgres.listUsers().catch(() => []),
        agent.postgres.listDatabases().catch(() => []),
        agent.postgres
          .pgvectorStatus()
          .catch(() => ({ available: false, enabled: [] as string[] })),
      ])
    : [[], [], { available: false, enabled: [] as string[] }];

  return (
    <div>
      <div className={styles.head}>
        <div>
          <h1 className={styles.title}>Databases</h1>
          <div className={styles.subtitle}>
            Two engines, two clearly-separated sections below. Every form
            in <strong>MariaDB</strong> talks to the local MariaDB socket;
            <strong> PostgreSQL</strong> runs against the local cluster via
            <code className={styles.code}> psql</code> (peer auth) — create,
            edit, and drop databases and roles right here.
          </div>
        </div>
      </div>

      {dbRecipes.length > 0 && (
        <div style={{ marginTop: 12, marginBottom: 24 }}>
          <QuickInstallStrip
            recipes={dbRecipes}
            serverAddress={serverAddress}
            heading="Database engines & GUIs"
            hint="MariaDB / PostgreSQL servers plus phpMyAdmin and pgAdmin web UIs. To log in from phpMyAdmin, create a MariaDB user with Host = % below."
            installAction={quickInstallAction}
            bindAction={setBindAction}
            getRoundcubeSettings={getRoundcubeSettingsAction}
            updateRoundcubeSettings={updateRoundcubeSettingsAction}
            getN8nSettings={getN8nSettingsAction}
            updateN8nSettings={updateN8nSettingsAction}
            getPhpMyAdminSettings={getPhpMyAdminSettingsAction}
            updatePhpMyAdminSettings={updatePhpMyAdminSettingsAction}
          />
        </div>
      )}

      {/*
        MariaDB engine group. Every action below (create database, create
        user, grant) runs against the MariaDB socket via
        agent.databases.*. The agent's error mapper even parses MariaDB
        error codes (1007/1008/1396). The visible engine header is what
        prevents the "is this Mariadb or Postgres?" confusion the page
        previously had — sections used to read just "Create database"
        with no engine cue beyond the page title.
      */}
      <EngineHeader
        slug="mariadb"
        name="MariaDB"
        installed={mariadb?.installed ?? false}
        description="User and database management on the local MariaDB instance. All actions run as root@localhost via the mysql socket."
      />

      {/* Create + import side by side; auto-fit drops to one column when the
          content area is too narrow for two ~360px cards (phones, split view). */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(360px, 1fr))",
          gap: 16,
          alignItems: "stretch",
          marginBottom: 18,
        }}
      >
        <div className={styles.section} style={{ marginBottom: 0 }}>
          <h2 className={styles.sectionTitle}>Create MariaDB database</h2>
          <CreateDbForm action={createDatabaseAction} />
        </div>

        {/*
          SQL dump import. The whole restore path in one form: the dump
          streams into Drive's sql-dumps/ via the existing /api/files/upload
          route (reusing the one nginx location with the multi-GB body
          carve-out), then the agent pipes it through the mariadb CLI as a
          background task — see agent/src/handlers/databases.rs::import_sql.
        */}
        <div className={styles.section} style={{ marginBottom: 0 }}>
          <h2 className={styles.sectionTitle}>Import SQL dump</h2>
          <ImportSqlForm
            dbs={dbs}
            maxUploadMb={uploadLimitMb()}
            prepareAction={prepareSqlImportAction}
            importAction={importSqlDumpAction}
          />
        </div>
      </div>

      <Table
        columns={[
          { key: "name", label: "MariaDB database" },
          { key: "users", label: "Granted users" },
          { key: "actions", label: "", width: "100px" },
        ]}
        rows={dbs.map((d) => ({
          name: <span className={styles.code}>{d.name}</span>,
          users: (
            <GrantedUsers
              database={d.name}
              users={d.users}
              revokeAction={revokeDbGrantAction}
            />
          ),
          actions: (
            <ConfirmDeleteButton
              action={deleteDatabaseAction.bind(null, d.name)}
              buttonLabel="Drop"
              confirmLabel="Drop database"
              title={`Drop MariaDB database ${d.name}?`}
              body={
                <p>
                  This permanently deletes the database{" "}
                  <span className={styles.code}>{d.name}</span> and{" "}
                  <strong>all tables and data inside it</strong>. This cannot
                  be undone.
                </p>
              }
            />
          ),
        }))}
        empty="No MariaDB databases yet."
      />

      <div className={styles.section} style={{ marginTop: 18 }}>
        <h2 className={styles.sectionTitle}>Create MariaDB user</h2>
        <CreateUserForm action={createDbUserAction} />
      </div>

      <Table
        columns={[
          { key: "user", label: "MariaDB user" },
          { key: "host", label: "Host" },
          { key: "actions", label: "", width: "200px" },
        ]}
        rows={users.map((u) => ({
          user: <span className={styles.code}>{u.user}</span>,
          host: u.host,
          actions: (
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <ChangePasswordButton
                identity={`${u.user}@${u.host}`}
                action={setDbUserPasswordAction.bind(null, u.user, u.host)}
              />
              <ConfirmDeleteButton
                action={deleteDbUserAction.bind(null, u.user, u.host)}
                buttonLabel="Drop"
                confirmLabel="Drop user"
                title={`Drop MariaDB user ${u.user}@${u.host}?`}
                body={
                  <p>
                    This permanently removes the user{" "}
                    <span className={styles.code}>{u.user}</span> and all its
                    grants. Apps connecting as this user will stop
                    authenticating. This cannot be undone.
                  </p>
                }
              />
            </div>
          ),
        }))}
        empty="No MariaDB users yet."
      />

      <div className={styles.section} style={{ marginTop: 18 }}>
        <h2 className={styles.sectionTitle}>Grant MariaDB privileges</h2>
        <GrantForm action={grantAction} users={users} databases={dbs} />
      </div>

      {/*
        PostgreSQL engine group. Mirrors the MariaDB structure: create
        database, list databases with drop, create role, list roles with
        drop, grant privileges. All actions shell out to `sudo -u
        postgres psql` on the agent via peer auth — no connection
        credentials are needed here.

        Layered grant model: the agent's grant handler doesn't only
        GRANT on the database (which by itself doesn't actually let the
        role create or query tables, a common Postgres trap). It also
        cascades into the public schema and existing tables/sequences
        for ALL PRIVILEGES — see agent/src/handlers/postgres.rs::grant.
      */}
      <EngineHeader
        slug="postgresql"
        name="PostgreSQL"
        installed={postgres?.installed ?? false}
        description="Role and database management on the local PostgreSQL instance. The agent runs psql as the postgres unix user via peer auth — no password needed."
      />

      {postgres?.installed ? (
        <>
          <div className={styles.section}>
            <h2 className={styles.sectionTitle}>Create Postgres database</h2>
            <CreatePgDbForm action={createPgDatabaseAction} roles={pgRoles} />
          </div>

          <Table
            columns={[
              { key: "name", label: "Postgres database" },
              { key: "owner", label: "Owner" },
              { key: "pgvector", label: "pgvector", width: "150px" },
              { key: "actions", label: "", width: "160px" },
            ]}
            rows={pgDbs.map((d) => ({
              name: <span className={styles.code}>{d.name}</span>,
              owner: <span className={styles.code}>{d.owner}</span>,
              pgvector: (
                <PgVectorButton
                  database={d.name}
                  enabled={pgvector.enabled.includes(d.name)}
                  available={pgvector.available}
                  enableAction={enablePgvectorAction}
                  disableAction={disablePgvectorAction}
                />
              ),
              actions: (
                <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                  <PgDbEditButton
                    db={d}
                    roles={pgRoles.map((r) => r.user)}
                    action={updatePgDatabaseAction}
                  />
                  <ConfirmDeleteButton
                    action={deletePgDatabaseAction.bind(null, d.name)}
                    buttonLabel="Drop"
                    confirmLabel="Drop database"
                    title={`Drop Postgres database ${d.name}?`}
                    body={
                      <p>
                        This permanently deletes the database{" "}
                        <span className={styles.code}>{d.name}</span> and{" "}
                        <strong>all tables and data inside it</strong>. Active
                        connections are terminated. This cannot be undone.
                      </p>
                    }
                  />
                </div>
              ),
            }))}
            empty="No Postgres databases yet."
          />

          <div className={styles.section} style={{ marginTop: 18 }}>
            <h2 className={styles.sectionTitle}>Create Postgres role</h2>
            <CreatePgUserForm action={createPgRoleAction} />
          </div>

          <Table
            columns={[
              { key: "user", label: "Postgres role" },
              { key: "flags", label: "Flags" },
              { key: "actions", label: "", width: "200px" },
            ]}
            rows={pgRoles.map((r) => ({
              user: <span className={styles.code}>{r.user}</span>,
              flags: r.superuser ? (
                <StatusBadge text="superuser" tone="warn" />
              ) : (
                <span className={styles.muted}>—</span>
              ),
              actions: (
                <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                  <ChangePasswordButton
                    identity={r.user}
                    action={setPgRolePasswordAction.bind(null, r.user)}
                  />
                  <ConfirmDeleteButton
                    action={deletePgRoleAction.bind(null, r.user)}
                    buttonLabel="Drop"
                    confirmLabel="Drop role"
                    title={`Drop Postgres role ${r.user}?`}
                    body={
                      <p>
                        This removes the role{" "}
                        <span className={styles.code}>{r.user}</span>. Objects it
                        owns are reassigned to{" "}
                        <span className={styles.code}>postgres</span> first so the
                        drop can proceed. This cannot be undone.
                      </p>
                    }
                  />
                </div>
              ),
            }))}
            empty="No Postgres roles yet."
          />

          <div className={styles.section} style={{ marginTop: 18 }}>
            <h2 className={styles.sectionTitle}>Grant Postgres privileges</h2>
            <PgGrantForm
              action={pgGrantAction}
              roles={pgRoles}
              databases={pgDbs}
            />
          </div>
        </>
      ) : (
        <div className={styles.section}>
          <div className={styles.muted}>
            PostgreSQL is not installed on this server. Install it from the
            strip above to enable role and database management here.
          </div>
        </div>
      )}

      {pgadmin?.installed && (
        // The quick-install recipe bakes default credentials into the
        // dpage/pgadmin4 container (PGADMIN_DEFAULT_EMAIL /
        // PGADMIN_DEFAULT_PASSWORD env vars in
        // agent/src/handlers/quick_install.rs::PGADMIN). The panel doesn't
        // surface them anywhere else, so operators have no way to log in
        // on the first visit without reading the source — that's the gap
        // this notice closes. Tone is `warn` because the password is the
        // string "changeme" — leaving it in place would let anyone who
        // can reach the port (or any future-bound public IP) own the
        // Postgres GUI. Rendered as a top-level alert (not wrapped in a
        // .section card) so it doesn't visually duplicate the engine
        // group above.
        <div className={styles.section}>
          <div
            style={{
              padding: "10px 14px",
              background: "rgba(255, 196, 0, 0.08)",
              border: "1px solid rgba(255, 196, 0, 0.32)",
              borderRadius: 8,
              fontSize: 13,
              lineHeight: 1.5,
            }}
          >
            <strong>pgAdmin first-login credentials</strong>
            <div style={{ marginTop: 6 }}>
              Email{" "}
              <code className={styles.code}>admin@example.com</code>, password{" "}
              <code className={styles.code}>changeme</code>. These are baked
              into the container by the install recipe.{" "}
              <strong>Change the password immediately</strong> from the
              pgAdmin top-right user menu &rarr; <em>Change Password</em>. The{" "}
              <code className={styles.code}>pgadmin_data</code> Docker volume
              persists the new password across restarts.
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * Big visual divider that labels the engine each sub-section operates
 * on. Placed at the top of the MariaDB group and again at the top of
 * the PostgreSQL group so the page reads as "two stacked engine apps"
 * rather than a flat list where MariaDB-specific forms could be
 * mistaken for Postgres ones (the actual bug the operator hit).
 *
 * Layout: engine icon + name + installed/not-installed pill, with the
 * description below. Uses an h2 so the screen-reader / outline still
 * surfaces these as primary page landmarks.
 */
function EngineHeader({
  slug,
  name,
  installed,
  description,
}: {
  slug: string;
  name: string;
  installed: boolean;
  description: string;
}) {
  return (
    <div
      style={{
        marginTop: 28,
        marginBottom: 14,
        paddingBottom: 10,
        borderBottom: "1px solid var(--border)",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          marginBottom: 4,
          flexWrap: "wrap",
        }}
      >
        <ServiceIcon slug={slug} size={24} />
        <h2
          style={{
            margin: 0,
            fontSize: 18,
            fontWeight: 600,
            letterSpacing: "-0.01em",
          }}
        >
          {name}
        </h2>
        <StatusBadge
          text={installed ? "installed" : "not installed"}
          tone={installed ? "ok" : "muted"}
        />
      </div>
      <div className={styles.muted} style={{ fontSize: 12, marginLeft: 36 }}>
        {description}
      </div>
    </div>
  );
}
