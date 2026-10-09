# Bolt Panel - first install

You need:

- A fresh **Ubuntu 24.04** server you can reach as root over SSH (install.sh refuses any other release).
- Your own SSH keypair, private and public half, **without a passphrase** (the panel runs git without prompts and cannot type one).
- That public key on GitHub with read access to `Meliorate-agency/vps-panel`: the repo's Settings > Deploy keys (read-only is enough), or SSH keys on a GitHub account that can read the repo. GitHub takes a deploy key on one repository only.

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

# 2. Put your keypair on the server

On the server the files are always named `github_vps_panel` and `github_vps_panel.pub`, whatever the key is called on your PC. The alias in step 3 and the clone URL in step 6 use that name.

```bash
mkdir -p -m 700 ~/.ssh
```

Private key: run this, paste the whole key (from the `-----BEGIN` line to the `-----END` line), press Enter, then Ctrl+D.

```bash
cat > ~/.ssh/github_vps_panel
```

Public key: the same, with the single `ssh-ed25519 AAAA...` (or `ssh-rsa AAAA...`) line.

```bash
cat > ~/.ssh/github_vps_panel.pub
```

Or copy both files from your PC instead of pasting. In PowerShell on the PC, replacing `YOUR_KEY` and `SERVER_IP`:

```powershell
scp $HOME\.ssh\YOUR_KEY root@SERVER_IP:/root/.ssh/github_vps_panel
scp $HOME\.ssh\YOUR_KEY.pub root@SERVER_IP:/root/.ssh/github_vps_panel.pub
```

Back on the server, set the permissions and check that the two halves belong together:

```bash
chmod 600 ~/.ssh/github_vps_panel && chmod 644 ~/.ssh/github_vps_panel.pub && [ "$(ssh-keygen -y -f ~/.ssh/github_vps_panel | cut -d' ' -f2)" = "$(cut -d' ' -f2 ~/.ssh/github_vps_panel.pub)" ] && echo "pair OK" || echo "MISMATCH"
```

- `pair OK`: go on.
- `Load key ... error in libcrypto` or `invalid format`: the private key paste was cut short or mangled. Paste it again.
- `MISMATCH` alone: that public key does not belong to this private key.
- It asks for a passphrase: remove it with `ssh-keygen -p -f ~/.ssh/github_vps_panel` (old passphrase, then Enter twice).

---

# 3. GitHub alias

ssh uses this key only for remotes written as `git@github_vps_panel:...`. This Host block is what makes that work.

```bash
printf '%s\n' 'Host github_vps_panel' '  HostName github.com' '  User git' '  IdentityFile ~/.ssh/github_vps_panel' '  IdentitiesOnly yes' >> ~/.ssh/config
chmod 600 ~/.ssh/config
```

---

# 4. Optional: log in to this server with the same key

Appends, so a key your provider already put there keeps working.

```bash
cat ~/.ssh/github_vps_panel.pub >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

---

# 5. Test GitHub access

Type `yes` when it asks about GitHub's host key.

```bash
ssh -T git@github_vps_panel
```

- `Hi Meliorate-agency/vps-panel! You've successfully authenticated...`: good (deploy key).
- `Hi <your GitHub user>! You've successfully authenticated...`: good (account key).
- Any other repo name: the key is that repo's deploy key and cannot read vps-panel.
- `Permission denied (publickey)`: the public key is not on GitHub yet.

---

# 6. Clone Bolt Panel

Always through the alias. Never `git@github.com:...` and never `git clone -c core.sshCommand=...`: the panel runs git with its own `GIT_SSH_COMMAND`, which overrides `core.sshCommand`, so such a clone installs fine and then every self-update fails with `Permission denied (publickey)`.

```bash
git clone git@github_vps_panel:Meliorate-agency/vps-panel.git /root/vps-panel-src
```

---

# 7. Install and run Bolt Panel

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

# 8. Check self-update

This runs git the way the panel does. No output means the Update button will work.

```bash
GIT_SSH_COMMAND="ssh -o BatchMode=yes" git -C /root/vps-panel-src fetch
```

If self-update ever fails with `Permission denied (publickey)`, look at the remote:

```bash
git -C /root/vps-panel-src remote -v
```

It must say `git@github_vps_panel:...`. If it says `git@github.com:...`, switch it:

```bash
git -C /root/vps-panel-src remote set-url origin git@github_vps_panel:Meliorate-agency/vps-panel.git
```
