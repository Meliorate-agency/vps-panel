"use client";

import { useActionState, useEffect, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import {
  advanceSetupStepAction,
  completeSetupAction,
  setFail2banPolicyAction,
  triggerAptUpgradeAction,
} from "@/server-actions/setup";
import type { SetupStep } from "./steps";
import { setAcmeEmailAction } from "@/server-actions/acme";
import { setSettingAction } from "@/server-actions/settings";
import {
  setPanelHostAction,
  completePanelHostSetupAction,
} from "@/server-actions/panelHost";
import { changePasswordAction } from "@/server-actions/auth";
import { addSshKeyAction, setSshServerConfigAction } from "@/server-actions/ssh";
import SshServerForm from "@/app/(panel)/settings/SshServerForm";
import { quickInstallAction, type QuickInstallResult } from "@/server-actions/quickInstall";
import AcmeEmailForm from "@/app/(panel)/settings/AcmeEmailForm";
import NameserverPairForm from "@/app/(panel)/settings/NameserverPairForm";
import ChangePasswordForm from "@/app/(panel)/settings/ChangePasswordForm";
import PanelHostForm from "@/app/(panel)/settings/PanelHostForm";
import TaskProgress from "@/components/TaskProgress/TaskProgress";
import TailscaleManager from "@/components/QuickInstallStrip/TailscaleManager";
import shared from "@/components/page.module.css";
import styles from "./setup.module.css";
import type { CoreServiceState, SshServerConfig } from "@/lib/agent";
import { PHASE_LABEL, phaseTone, type TailscaleCard } from "@/lib/tailscale";
import type { CatalogEntry } from "./catalog";

type Policy = {
  bantime: string;
  findtime: string;
  maxretry: number;
  jails: string[];
};

type Props = {
  steps: SetupStep[];
  initialStep: SetupStep;
  panelHost: string;
  adminEmail: string;
  acmeEmail: string;
  nsPair: string;
  policy: Policy;
  catalog: CatalogEntry[];
  /** Quick-install slugs already installed when the wizard loaded. */
  installedApps: string[];
  initialServices: CoreServiceState[];
  managedZones: string[];
  /** Live sshd settings; null when they could not be read. */
  sshConfig: SshServerConfig | null;
  /** SSH keys in root's authorized_keys. */
  rootKeyCount: number;
  /** Tailscale's phase and tailnet IP while it is installed, else null. */
  tailscale: TailscaleCard | null;
};

const STEP_LABELS: Record<SetupStep, string> = {
  welcome: "Welcome",
  hostname: "Hostname",
  services: "Services",
  acme: "SSL email",
  nameservers: "Nameservers",
  security: "Security",
  apps: "Apps",
  password: "Password",
};

export default function Wizard(props: Props) {
  const [step, setStep] = useState<SetupStep>(props.initialStep);
  const [, startAdvance] = useTransition();

  const goTo = (next: SetupStep) => {
    setStep(next);
    startAdvance(async () => {
      await advanceSetupStepAction(next);
    });
  };

  const idx = props.steps.indexOf(step);
  const next = (): SetupStep | null =>
    idx >= 0 && idx < props.steps.length - 1 ? props.steps[idx + 1] : null;
  // Back follows SETUP_STEPS, so reordering the steps there is enough.
  const prev = (): SetupStep => props.steps[Math.max(idx - 1, 0)];

  return (
    <>
      <div className={styles.header}>
        <div className={styles.heading}>First-run setup</div>
        <div className={styles.sub}>
          One-time configuration for {props.panelHost}. Step {idx + 1} of {props.steps.length}
          {" — "}
          {STEP_LABELS[step]}.
        </div>
        <div className={styles.stepper} style={{ marginTop: 8 }}>
          {props.steps.map((s, i) => (
            <span
              key={s}
              className={`${styles.stepDot} ${
                i < idx ? styles.stepDotDone : i === idx ? styles.stepDotCurrent : ""
              }`}
              title={STEP_LABELS[s]}
            />
          ))}
        </div>
      </div>

      {step === "welcome" && (
        <WelcomeStep
          panelHost={props.panelHost}
          adminEmail={props.adminEmail}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "acme" && (
        <AcmeStep
          initial={props.acmeEmail}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "hostname" && (
        <HostnameStep
          initial={props.panelHost}
          managedZones={props.managedZones}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "services" && (
        <ServicesStep
          initial={props.initialServices}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "nameservers" && (
        <NameserversStep
          initial={props.nsPair}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "security" && (
        <SecurityStep
          initial={props.policy}
          sshConfig={props.sshConfig}
          rootKeyCount={props.rootKeyCount}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "apps" && (
        <AppsStep
          catalog={props.catalog}
          installedApps={props.installedApps}
          tailscale={props.tailscale}
          onBack={() => goTo(prev())}
          onNext={() => goTo(next()!)}
        />
      )}
      {step === "password" && (
        <PasswordStep
          onBack={() => goTo(prev())}
        />
      )}
    </>
  );
}

function WelcomeStep({
  panelHost,
  adminEmail,
  onNext,
}: {
  panelHost: string;
  adminEmail: string;
  onNext: () => void;
}) {
  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Welcome</h2>
      <p className={styles.cardSub}>
        The installer brought the panel online. We&apos;ll walk through the choices that
        weren&apos;t made for you yet: who Let&apos;s Encrypt contacts, the panel&apos;s
        hostname, which services are healthy, your nameserver pair, brute-force thresholds
        and SSH login, optional apps such as Tailscale, and a new admin password.
      </p>
      <ul style={{ display: "flex", flexDirection: "column", gap: 6, listStyle: "none" }}>
        <li>
          <span className={shared.muted}>Panel host:</span>{" "}
          <span className={shared.code}>{panelHost}</span>
        </li>
        <li>
          <span className={shared.muted}>Admin email:</span>{" "}
          <span className={shared.code}>{adminEmail || "(not set in /etc/vps-panel/.env)"}</span>
        </li>
      </ul>
      <div className={styles.footer}>
        <span className={shared.muted}>You can&apos;t reach the dashboard until this is done.</span>
        <button className={shared.btn} onClick={onNext}>
          Get started
        </button>
      </div>
    </section>
  );
}

function HostnameStep({
  initial,
  managedZones,
  onBack,
  onNext,
}: {
  initial: string;
  managedZones: string[];
  onBack: () => void;
  onNext: () => void;
}) {
  const looksLikeIp = /^[0-9.]+$/.test(initial);
  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Panel hostname</h2>
      <p className={styles.cardSub}>
        {looksLikeIp || !initial ? (
          <>
            Right now the panel answers on{" "}
            <span className={shared.code}>{`http://${initial || "<server-ip>"}`}</span>{" "}
            with non-Secure cookies. Set a real FQDN here to switch nginx, Postfix,
            and Dovecot over to it (and unlock HTTPS later). Skip to keep IP-only
            mode — you can do this from Settings any time.
          </>
        ) : (
          <>
            Currently set to <span className={shared.code}>{initial}</span>. Change
            it to point the panel at a different domain, or clear it to revert to
            IP-only mode.
          </>
        )}
      </p>
      <PanelHostForm
        value={initial}
        saveAction={setPanelHostAction}
        completeAction={completePanelHostSetupAction}
        managedZones={managedZones}
      />
      <p className={shared.muted} style={{ fontSize: 12, marginTop: 12 }}>
        Saving rewrites the nginx vhost, Postfix <span className={shared.code}>myhostname</span>,
        Dovecot <span className={shared.code}>postmaster_address</span>, and the
        env file. The panel then restarts in a few seconds — you&apos;ll need to
        reopen it and log in again before resuming the wizard: at the new URL, or by
        the server&apos;s IP until the name&apos;s DNS points here.
      </p>
      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <div className={styles.footerRight}>
          <button className={shared.btnGhost} onClick={onNext}>
            Skip
          </button>
          <button className={shared.btn} onClick={onNext}>
            Continue
          </button>
        </div>
      </div>
    </section>
  );
}

function ServicesStep({
  initial,
  onBack,
  onNext,
}: {
  initial: CoreServiceState[];
  onBack: () => void;
  onNext: () => void;
}) {
  const [services, setServices] = useState<CoreServiceState[]>(initial);
  const [refreshing, setRefreshing] = useState(false);
  const [installState, installFormAction, installPending] = useActionState<
    QuickInstallResult | null,
    FormData
  >(quickInstallAction, null);

  async function refresh() {
    setRefreshing(true);
    try {
      const res = await fetch("/api/setup/services", { cache: "no-store" });
      if (res.ok) setServices(await res.json());
    } finally {
      setRefreshing(false);
    }
  }

  useEffect(() => {
    // Auto-refresh while a repair task is in flight so freshly installed
    // services flip to healthy without the operator having to hit the button.
    if (!installState?.ok || !installState.data?.taskId) return;
    const t = setInterval(refresh, 4000);
    return () => clearInterval(t);
  }, [installState]);

  const allOk = services.every((s) => s.installed && s.active);

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Service health</h2>
      <p className={styles.cardSub}>
        The installer should have brought all of these up. Repair anything that&apos;s red
        before continuing.
      </p>

      <div className={styles.checkRow}>
        {services.map((s) => (
          <div key={s.name} className={styles.serviceRow}>
            <div>
              <div className={styles.serviceName}>{s.name}</div>
              <div className={styles.serviceMeta}>
                unit <span className={shared.code}>{s.unit}</span>
                {s.package ? (
                  <>
                    {" "}/ pkg <span className={shared.code}>{s.package}</span>
                  </>
                ) : null}
              </div>
            </div>
            <div className={styles.row}>
              <span
                className={`${styles.statusPill} ${
                  s.installed ? styles.statusOk : styles.statusBad
                }`}
              >
                {s.installed ? "installed" : "missing"}
              </span>
              <span
                className={`${styles.statusPill} ${
                  s.active ? styles.statusOk : styles.statusWarn
                }`}
              >
                {s.active ? "active" : "down"}
              </span>
            </div>
            <div>
              {!s.installed && s.quick_install_slug ? (
                <form action={installFormAction}>
                  <input type="hidden" name="slug" value={s.quick_install_slug} />
                  <button
                    type="submit"
                    className={shared.btnGhost}
                    disabled={installPending}
                  >
                    {installPending ? "..." : "Install"}
                  </button>
                </form>
              ) : null}
            </div>
          </div>
        ))}
      </div>

      {installState?.ok === false && (
        <div className={shared.error} style={{ marginTop: 12 }}>
          {installState.error}
        </div>
      )}
      {installState?.ok === true && installState.data?.taskId && (
        <div style={{ marginTop: 12 }}>
          <TaskProgress
            taskId={installState.data.taskId}
            onComplete={() => refresh()}
          />
        </div>
      )}

      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <div className={styles.footerRight}>
          <button className={shared.btnGhost} onClick={refresh} disabled={refreshing}>
            {refreshing ? "..." : "Re-check"}
          </button>
          <button
            className={shared.btn}
            onClick={onNext}
            title={allOk ? "" : "Continue anyway — you can repair later from Services."}
          >
            {allOk ? "Continue" : "Continue anyway"}
          </button>
        </div>
      </div>
    </section>
  );
}

function AcmeStep({
  initial,
  onBack,
  onNext,
}: {
  initial: string;
  onBack: () => void;
  onNext: () => void;
}) {
  const [saved, setSaved] = useState(initial !== "");
  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>SSL contact email</h2>
      <p className={styles.cardSub}>
        Let&apos;s Encrypt sends expiry and security notices here. Must be a real
        address with a dotted domain. Falls back to <span className={shared.code}>ACME_EMAIL</span>{" "}
        in <span className={shared.code}>/etc/vps-panel/.env</span>{" "}
        if you leave it blank. It comes before the hostname because that step can issue the
        panel&apos;s certificate, and Let&apos;s Encrypt registers it with this address.
      </p>
      <AcmeEmailForm
        value={initial}
        action={async (next) => {
          const r = await setAcmeEmailAction(next);
          if (r.ok) setSaved(true);
          return r;
        }}
      />
      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <button className={shared.btn} onClick={onNext} disabled={!saved}>
          Continue
        </button>
      </div>
    </section>
  );
}

function NameserversStep({
  initial,
  onBack,
  onNext,
}: {
  initial: string;
  onBack: () => void;
  onNext: () => void;
}) {
  const [saved, setSaved] = useState(initial !== "");
  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>DNS nameservers</h2>
      <p className={styles.cardSub}>
        Authoritative nameservers used when the panel creates a PowerDNS zone, e.g.{" "}
        <span className={shared.code}>ns1.example.com</span> and{" "}
        <span className={shared.code}>ns2.example.com</span>. Make sure glue records at your
        registrar point both names back at this server&apos;s public IP. Leave both blank to
        fall back to per-zone defaults.
      </p>
      <NameserverPairForm
        value={initial}
        action={async (next) => {
          const r = await setSettingAction("dns.ns_pair", next);
          if (r.ok) setSaved(true);
          return r;
        }}
      />
      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <div className={styles.footerRight}>
          {!saved && (
            <button
              className={shared.btnGhost}
              onClick={onNext}
              title="Skip — you can configure nameservers later in Settings."
            >
              Skip
            </button>
          )}
          <button className={shared.btn} onClick={onNext} disabled={!saved}>
            Continue
          </button>
        </div>
      </div>
    </section>
  );
}

function SecurityStep({
  initial,
  sshConfig,
  rootKeyCount,
  onBack,
  onNext,
}: {
  initial: Policy;
  sshConfig: SshServerConfig | null;
  rootKeyCount: number;
  onBack: () => void;
  onNext: () => void;
}) {
  const [state, formAction, pending] = useActionState(setFail2banPolicyAction, null);
  const [aptTaskId, setAptTaskId] = useState<string | null>(null);
  const [aptError, setAptError] = useState<string | null>(null);
  const [aptStarting, startApt] = useTransition();

  const triggerApt = () => {
    setAptError(null);
    startApt(async () => {
      const r = await triggerAptUpgradeAction();
      if (r.ok) setAptTaskId(r.data.taskId);
      else setAptError(r.error);
    });
  };

  // Mirrors KNOWN_JAILS in agent/src/handlers/firewall.rs.
  const allJails = ["sshd", "postfix-sasl", "dovecot"];

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Security defaults</h2>
      <p className={styles.cardSub}>
        Tune the fail2ban thresholds and pick which jails to enable. Also a good
        moment to apply pending OS package updates.
      </p>

      <form action={formAction} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        <div className={shared.formRow}>
          <label className={shared.field}>
            Ban time
            <input
              name="bantime"
              className={shared.input}
              defaultValue={initial.bantime}
              placeholder="1h"
              required
            />
          </label>
          <label className={shared.field}>
            Find time
            <input
              name="findtime"
              className={shared.input}
              defaultValue={initial.findtime}
              placeholder="10m"
              required
            />
          </label>
          <label className={shared.field}>
            Max retries
            <input
              name="maxretry"
              type="number"
              min={1}
              max={1000}
              className={shared.input}
              defaultValue={initial.maxretry}
              required
            />
          </label>
        </div>

        <div className={shared.field}>
          Jails to enable
          <div className={styles.checkRow}>
            {allJails.map((jail) => (
              <label key={jail} className={shared.checkbox}>
                <input
                  type="checkbox"
                  name="jails"
                  value={jail}
                  defaultChecked={initial.jails.includes(jail)}
                />
                {jail}
              </label>
            ))}
          </div>
        </div>

        {state?.ok === false && <div className={shared.error}>{state.error}</div>}
        {state?.ok === true && <div className={shared.notice}>fail2ban policy applied.</div>}
        <div>
          <button type="submit" className={shared.btn} disabled={pending}>
            {pending ? "..." : "Save fail2ban policy"}
          </button>
        </div>
      </form>

      <hr style={{ border: 0, borderTop: "1px solid var(--border)", margin: "20px 0" }} />

      <SshLoginSetup config={sshConfig} rootKeyCount={rootKeyCount} />

      <hr style={{ border: 0, borderTop: "1px solid var(--border)", margin: "20px 0" }} />

      <div>
        <div className={shared.sectionTitle}>System packages</div>
        <p className={shared.muted} style={{ marginBottom: 10 }}>
          Runs <span className={shared.code}>apt-get update &amp;&amp; apt-get -y upgrade</span> on
          the host. Safe to skip — unattended-upgrades will catch up later.
        </p>
        <div className={styles.row}>
          <button
            className={shared.btn}
            onClick={triggerApt}
            disabled={aptStarting || aptTaskId !== null}
          >
            {aptStarting ? "..." : aptTaskId ? "Running" : "Update system"}
          </button>
        </div>
        {aptError && (
          <div className={shared.error} style={{ marginTop: 10 }}>
            {aptError}
          </div>
        )}
        {aptTaskId && (
          <div style={{ marginTop: 12 }}>
            <TaskProgress taskId={aptTaskId} />
          </div>
        )}
      </div>

      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <div className={styles.footerRight}>
          <button
            className={shared.btnGhost}
            onClick={onNext}
            title="Skip — you can configure fail2ban later in Firewall settings."
          >
            Skip
          </button>
          <button className={shared.btn} onClick={onNext}>
            Continue
          </button>
        </div>
      </div>
    </section>
  );
}

/**
 * SSH login in the wizard. The install leaves password and root login on
 * (servers are often provisioned without a key); here the suggested setting
 * is off: password login off, root by key only. Nothing changes until the
 * operator applies it, and Settings > SSH server can turn it back on.
 */
function SshLoginSetup({
  config,
  rootKeyCount,
}: {
  config: SshServerConfig | null;
  rootKeyCount: number;
}) {
  const [keyState, keyAction, keyPending] = useActionState(addSshKeyAction, null);
  if (!config) {
    return (
      <div>
        <div className={shared.sectionTitle}>SSH login</div>
        <p className={shared.muted}>
          Could not read the SSH server settings. Set them later in Settings, SSH server.
        </p>
      </div>
    );
  }
  return (
    <div>
      <div className={shared.sectionTitle}>SSH login</div>
      <p className={shared.muted} style={{ marginBottom: 10 }}>
        Recommended: no password login, root by SSH key only. Right now password login is{" "}
        {config.password_auth ? "on" : "off"} and root login is{" "}
        <span className={shared.code}>{config.permit_root_login}</span>. You can change it any
        time in Settings, SSH server.
      </p>
      {rootKeyCount === 0 ? (
        <div className={shared.error} style={{ marginBottom: 10 }}>
          root has no SSH key yet. Add your public key here first: with password login off and
          no key, SSH only works through your provider&apos;s web console.
        </div>
      ) : (
        <p className={shared.muted} style={{ marginBottom: 10 }}>
          root has {rootKeyCount} SSH key{rootKeyCount === 1 ? "" : "s"}.
        </p>
      )}
      <form action={keyAction} style={{ marginBottom: 14 }}>
        <input type="hidden" name="user" value="root" />
        <label className={shared.field}>
          Add a public key for root
          <textarea
            name="key"
            rows={2}
            className={shared.textarea}
            placeholder="ssh-ed25519 AAAA... you@laptop"
            required
          />
        </label>
        {keyState?.ok === false && <div className={shared.error}>{keyState.error}</div>}
        {keyState?.ok === true && <div className={shared.notice}>Key added.</div>}
        <button type="submit" className={shared.btnGhost} disabled={keyPending}>
          {keyPending ? "Adding..." : "Add key"}
        </button>
      </form>
      <SshServerForm
        config={config}
        action={setSshServerConfigAction}
        loginOnly
        suggestPasswordAuth={false}
        suggestRootLogin={config.permit_root_login === "no" ? "no" : "prohibit-password"}
      />
    </div>
  );
}

function AppsStep({
  catalog,
  installedApps,
  tailscale,
  onBack,
  onNext,
}: {
  catalog: CatalogEntry[];
  installedApps: string[];
  tailscale: TailscaleCard | null;
  onBack: () => void;
  onNext: () => void;
}) {
  const router = useRouter();
  // Installed from this step, Tailscale has no card from the server until
  // the refresh below lands; a fresh install waits for its login.
  const ts: TailscaleCard = tailscale ?? { phase: "needs_login", ip: null };
  const tsTone = phaseTone(ts.phase);
  const [installState, installFormAction, installPending] = useActionState<
    QuickInstallResult | null,
    FormData
  >(quickInstallAction, null);
  // Installed = already installed when the step loaded, or an install task
  // from this step that finished successfully. Those rows show "Installed"
  // instead of an Install button.
  const [installed, setInstalled] = useState<Set<string>>(() => new Set(installedApps));
  // Outcome per install task id, filled in when TaskProgress reports the end.
  const [finished, setFinished] = useState<Record<string, "succeeded" | "failed">>({});
  const current = installState?.ok ? installState.data : undefined;
  const runningSlug =
    current?.taskId && current.slug && !finished[current.taskId] ? current.slug : null;
  const failedSlug =
    current?.taskId && current.slug && finished[current.taskId] === "failed"
      ? current.slug
      : null;
  // One install at a time: they share apt/docker and would trip over each other.
  const busy = installPending || runningSlug !== null;

  const onInstallDone = (status: "succeeded" | "failed") => {
    if (!current?.taskId || !current.slug) return;
    const { taskId, slug } = current;
    setFinished((f) => ({ ...f, [taskId]: status }));
    if (status === "succeeded") {
      setInstalled((s) => new Set(s).add(slug));
      // Tailscale's phase comes from the server (setup page).
      if (slug === "tailscale") router.refresh();
    }
  };

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Optional apps</h2>
      <p className={styles.cardSub}>
        Hit Install on anything you want now. You can come back to Apps → Quick install
        any time. Tailscale gets a Log in button once it is installed.
      </p>

      <div className={styles.checkRow}>
        {catalog.map((c) => (
          <div key={c.slug} className={styles.checkItem}>
            <div className={styles.checkItemBody}>
              <div style={{ fontWeight: 600, fontSize: 13 }}>
                {c.label}
                {c.recommended ? (
                  <span
                    className={`${styles.statusPill} ${styles.statusOk}`}
                    style={{ marginLeft: 8 }}
                  >
                    recommended
                  </span>
                ) : null}
              </div>
              <div className={shared.muted} style={{ fontSize: 12 }}>
                {c.description}
              </div>
            </div>
            {installed.has(c.slug) && c.slug === "tailscale" ? (
              <div className={styles.row}>
                <span
                  className={`${styles.statusPill} ${
                    tsTone === "ok" ? styles.statusOk : tsTone === "warn" ? styles.statusWarn : ""
                  }`}
                >
                  {PHASE_LABEL[ts.phase]}
                  {ts.ip ? ` ${ts.ip}` : ""}
                </span>
                <TailscaleManager card={ts} className={shared.btnGhost} />
              </div>
            ) : installed.has(c.slug) ? (
              <span className={`${styles.statusPill} ${styles.statusOk}`}>Installed</span>
            ) : (
              <form action={installFormAction}>
                <input type="hidden" name="slug" value={c.slug} />
                <button type="submit" className={shared.btnGhost} disabled={busy}>
                  {runningSlug === c.slug
                    ? "Installing..."
                    : failedSlug === c.slug
                      ? "Retry"
                      : "Install"}
                </button>
              </form>
            )}
          </div>
        ))}
      </div>

      {installState?.ok === false && (
        <div className={shared.error} style={{ marginTop: 12 }}>
          {installState.error}
        </div>
      )}
      {failedSlug && (
        <div className={shared.error} style={{ marginTop: 12 }}>
          Installing {catalog.find((c) => c.slug === failedSlug)?.label ?? failedSlug} failed.
          The log below shows why; Retry runs it again.
        </div>
      )}
      {current?.taskId && (
        <div style={{ marginTop: 12 }}>
          <TaskProgress key={current.taskId} taskId={current.taskId} onComplete={onInstallDone} />
        </div>
      )}

      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <div className={styles.footerRight}>
          <button className={shared.btnGhost} onClick={onNext}>
            Skip
          </button>
          <button className={shared.btn} onClick={onNext}>
            Continue
          </button>
        </div>
      </div>
    </section>
  );
}

function PasswordStep({ onBack }: { onBack: () => void }) {
  const [changed, setChanged] = useState(false);
  const [finishing, startFinish] = useTransition();

  const wrappedAction = async (
    prev: { ok: true } | { ok: false; error: string } | null,
    fd: FormData,
  ) => {
    const r = await changePasswordAction(prev, fd);
    if (r.ok) setChanged(true);
    return r;
  };

  const finish = () => {
    startFinish(async () => {
      await completeSetupAction();
    });
  };

  return (
    <section className={styles.card}>
      <h2 className={styles.cardTitle}>Change admin password</h2>
      <p className={styles.cardSub}>
        The installer seeded a random password. Set your own before we finish. Minimum
        12 characters. After this you&apos;ll land on the dashboard.
      </p>
      <ChangePasswordForm action={wrappedAction} />
      <div className={styles.footer}>
        <button className={shared.btnGhost} onClick={onBack}>
          Back
        </button>
        <button
          className={shared.btn}
          onClick={finish}
          disabled={!changed || finishing}
        >
          {finishing ? "..." : "Finish setup"}
        </button>
      </div>
    </section>
  );
}
