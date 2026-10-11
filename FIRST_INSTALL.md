# Bolt Panel - first install

You need a fresh **Ubuntu 24.04** server you can reach as root over SSH (install.sh refuses any other release). The code comes from the public repo [Meliorate-agency/vps-panel](https://github.com/Meliorate-agency/vps-panel), so no GitHub key is needed.

Log in from your PC:

```bash
ssh root@SERVER_IP
```

Everything below runs on the server, as root.

---

# 1. Update Linux

```bash
apt update && apt upgrade -y && apt autoremove -y && apt autoclean
apt install -y git
lsb_release -a
```

There is no Node.js step: install.sh installs Node 22 itself.

---

# 2. Clone Bolt Panel

```bash
git clone https://github.com/Meliorate-agency/vps-panel.git /root/vps-panel-src
```

Keep it at `/root/vps-panel-src`: the panel updates itself from this clone.

---

# 3. Install and run Bolt Panel

```bash
bash /root/vps-panel-src/scripts/install.sh
```

It asks:

```text
Panel hostname (e.g. panel.example.com, or blank):
Admin email (or blank):
```

A blank hostname installs in IP-only mode over plain HTTP; set the real hostname later in the setup wizard or Settings. To skip the questions: `bash /root/vps-panel-src/scripts/install.sh --domain panel.example.com --email you@example.com`.

It builds the agent (Rust) and the panel (Next.js) from source, so it takes a while. At the end it prints the panel URL and the initial admin password. The first login opens the setup wizard.

Open the panel at `http://SERVER_IP/`, with plain `http`, even if you gave a domain whose DNS has not caught up yet. There is no https until you issue a cert (Settings > Issue cert), and that needs the domain's DNS to point here. If the browser switches to https by itself, type the `http://` again and choose to continue to the site. Port 10000 is the panel's internal port, bound to 127.0.0.1 behind nginx: do not open it.

---

# 4. Check self-update

The Update button in the panel pulls from this clone's GitHub remote and rebuilds. No output here means it can reach GitHub:

```bash
git -C /root/vps-panel-src fetch
```

---

# Installing from a private fork

Only for your own private copy of the repo: GitHub then needs a key before the server can clone it.

1. Put a keypair **without a passphrase** on the server as `/root/.ssh/github_vps_panel` and `/root/.ssh/github_vps_panel.pub` (the panel runs git without prompts), and add the public key to the fork's Settings > Deploy keys (read-only is enough).

   ```bash
   chmod 600 ~/.ssh/github_vps_panel && chmod 644 ~/.ssh/github_vps_panel.pub
   ```

2. Add an ssh alias so git uses that key, then test it (`Hi YOUR_ORG/YOUR_FORK! You've successfully authenticated...` is good):

   ```bash
   printf '%s\n' 'Host github_vps_panel' '  HostName github.com' '  User git' '  IdentityFile ~/.ssh/github_vps_panel' '  IdentitiesOnly yes' >> ~/.ssh/config
   chmod 600 ~/.ssh/config
   ssh -T git@github_vps_panel
   ```

3. Clone through the alias, then continue with step 3 above:

   ```bash
   git clone git@github_vps_panel:YOUR_ORG/YOUR_FORK.git /root/vps-panel-src
   ```

   Always the alias. Never `git@github.com:...` and never `git clone -c core.sshCommand=...`: the panel runs git with its own `GIT_SSH_COMMAND`, which overrides `core.sshCommand`, so such a clone installs fine and then every self-update fails with `Permission denied (publickey)`. If that happens, point the remote back at the alias with `git -C /root/vps-panel-src remote set-url origin git@github_vps_panel:YOUR_ORG/YOUR_FORK.git`.

Do not add a deploy key to `authorized_keys`: anyone who ever gets the key file could then log in to the server as root.
