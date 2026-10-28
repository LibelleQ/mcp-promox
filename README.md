# proxmox-mcp

MCP server (Rust, stdio) for Proxmox VE. It talks to the REST API with an API
token and lets Claude (or any MCP client) list, create, back up and rebuild
VMs and containers.

- 8 read-only tools, always available.
- 14 tools that modify Proxmox, **off by default** (`PROXMOX_ALLOW_WRITE=1`).
- Destructive actions require an explicit confirmation.

## 1. Requirements

- A Proxmox VE host reachable over HTTPS (port 8006).
- [Rust](https://rustup.rs) (edition 2024, so a recent toolchain).
- An MCP client: Claude Code, Claude Desktop, etc.

## 2. Build

```sh
cargo build --release
```

The binary is written to `target/release/proxmox-mcp` (`proxmox-mcp.exe` on
Windows). Note its absolute path, you need it in step 4.

## 3. Create the API tokens

On the Proxmox host (node shell), create a dedicated user and **two tokens**:
a read-only one for daily use and a second one for changes.

```sh
pveum user add mcp@pve

# Read-only token
pveum user token add mcp@pve ro --privsep 1
pveum acl modify / --tokens 'mcp@pve!ro' --roles PVEAuditor

# Write token
pveum user token add mcp@pve rw --privsep 1
pveum acl modify / --tokens 'mcp@pve!rw' --roles PVEVMAdmin,PVEDatastoreAdmin
```

Each `token add` prints the secret **once**. Copy it right away. The full token
looks like `mcp@pve!ro=<secret>`.

| Role | Used for |
|---|---|
| `PVEAuditor` | all read-only tools |
| `PVEVMAdmin` | create, clone, power, snapshot, back up and delete guests |
| `PVEDatastoreAdmin` | download ISO images and templates, delete stored files |

If creating a guest fails with a permission error on the network, the bridge
may also need the `SDN.Use` privilege on `/sdn/zones/localnetwork`.

## 4. Connect the MCP

### Environment variables

| Variable | Meaning |
|---|---|
| `PROXMOX_URL` | `https://<host>:8006` (`https://` is mandatory) |
| `PROXMOX_TOKEN` | `user@realm!tokenid=secret` |
| `PROXMOX_INSECURE=1` | accept a self-signed certificate (the default on a fresh Proxmox) |
| `PROXMOX_ALLOW_WRITE=1` | enable every tool that modifies Proxmox (off by default) |

### Claude Code

Read-only (daily use):

```sh
claude mcp add proxmox \
  --env PROXMOX_URL=https://192.168.1.10:8006 \
  --env PROXMOX_TOKEN='mcp@pve!ro=<secret>' \
  --env PROXMOX_INSECURE=1 \
  -- /path/to/proxmox-mcp
```

For changes: the same command with the `rw` token and
`--env PROXMOX_ALLOW_WRITE=1`. The safest setup is to register it under another
name (`proxmox-rw`) and only enable it when you need it.

### Claude Desktop

1. Build the release binary (`cargo build --release`), see step 2.
2. Open the config file: in Claude Desktop, *Settings → Developer → Edit Config*.
   On Windows the file is `%APPDATA%\Claude\claude_desktop_config.json`. If
   Claude was installed from the Microsoft Store, it may be under
   `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\` or
   `%LOCALAPPDATA%\Claude-Data\Claude_*\` instead: the *Edit Config* button
   opens the right folder.
3. Add the server:

   ```json
   {
     "mcpServers": {
       "proxmox": {
         "command": "C:\\path\\to\\proxmox-mcp.exe",
         "env": {
           "PROXMOX_URL": "https://192.168.1.10:8006",
           "PROXMOX_TOKEN": "mcp@pve!ro=<secret>",
           "PROXMOX_INSECURE": "1"
         }
       }
     }
   }
   ```

   Backslashes must be doubled in JSON. Start with the read-only token.
4. **Quit Claude Desktop completely**, then start it again. Closing the window
   is not enough: right-click the tray icon and choose *Quit*.
5. Check that it works. In *Settings → Developer*, `proxmox` should be listed
   as *running*. In a conversation, the tools icon under the input box lists
   the Proxmox tools. Then ask: "List my Proxmox nodes."

If the server is missing or in error, read the log
`%APPDATA%\Claude\logs\mcp-server-proxmox.log`. The usual causes are single
backslashes in the path, a missing comma in the JSON, or a `PROXMOX_URL`
without `https://` (the server refuses to start, by design).

Never commit a token: keep them in the client config, not in the repository.

## 5. Available tools

### Read-only

| Tool | Purpose |
|---|---|
| `list_nodes` | cluster nodes, status, resource usage |
| `list_guests` | every VM and container |
| `get_guest_status` | detailed status of one guest |
| `list_storages` | storages of a node and the content types they accept |
| `list_storage_content` | ISO images, templates, backups, disks of a storage |
| `list_lxc_catalog` | official container templates that can be downloaded |
| `list_snapshots` | snapshots of a guest |
| `get_task_status` | state of a task from its UPID |

### Modifying (`PROXMOX_ALLOW_WRITE=1`)

| Tool | Purpose |
|---|---|
| `download_iso` | download an ISO image (checksum mandatory) |
| `download_lxc_template` | download a container template |
| `create_container` | create a container (unprivileged by default). `nesting: true` enables the `nesting` feature, needed by Debian 13 (systemd 257) and Docker-in-LXC |
| `create_vm` | create a VM, optionally with an ISO |
| `clone_guest` | clone a template (full clone by default) |
| `convert_to_template` | turn a stopped guest into a template. **Irreversible** |
| `guest_power` | `start`, `shutdown`, `stop`, `reboot` |
| `snapshot_create` | take a snapshot |
| `snapshot_rollback` | roll back to a snapshot. **Loses everything done since** |
| `snapshot_delete` | delete a snapshot |
| `backup_guest` | vzdump backup, compressed with zstd |
| `restore_backup` | restore a backup |
| `delete_guest` | delete a guest and its disks |
| `delete_volume` | delete a stored backup, ISO image or container template |

Long operations (downloads, backups, clones) wait for the task to finish. If it
takes longer than the limit, the tool returns the UPID: follow it with
`get_task_status`.

A task that ends with `WARNINGS: N` is reported as a success with a "completed
with warnings" message, not as an error: Proxmox uses that status for harmless
notices (for example "You may need to enable nesting" on Debian 13 containers).
Read the message, and pass `nesting: true` to `create_container` if it applies.

## 6. Examples

You talk to Claude in plain language and it picks the tools.

**Explore**

> List my nodes and VMs, and tell me which ones are stopped.

> Which storages on node `pve` accept backups?

**Create a Debian container**

> Download the Debian 12 template on `local`, then create a container `web01`
> (id 120, 1 GB RAM, 2 cores, 10 GB disk on `local-lvm`) with my SSH key, and
> start it.

Claude chains `list_lxc_catalog`, `download_lxc_template`, `create_container`,
`guest_power`.

**Create a VM from an ISO**

> Download the Debian netinst ISO with its SHA256, then create a VM `lab01`
> with 4 GB of RAM using that ISO.

Provide the URL and checksum published by the distribution: `download_iso`
refuses to run without a checksum.

**Protect yourself before a risky change**

> Take a snapshot `before-upgrade` of container 120, then reboot it.

> That went wrong, roll back to `before-upgrade`.

**Back up**

> Back up container 120 to the `backup` storage.

## 7. Security

- **Read-only by default.** Without `PROXMOX_ALLOW_WRITE=1`, every modifying
  tool returns an error and never reaches Proxmox. The check lives in a single
  place (`ProxmoxClient::write`), so a new tool cannot forget it.
- **Two tokens.** Use the `PVEAuditor` token day to day. The `rw` token is only
  given to the MCP when you really want to change something.
- **Confirmations.** `delete_guest` requires `confirm_vmid` (the same ID
  repeated) and `delete_volume` requires `confirm_volid`. `restore_backup` only
  overwrites an existing guest with `force=true`.
- **Narrow deletions.** `delete_volume` only removes backups, ISO images and
  container templates. Guest disks can only be removed through `delete_guest`
  and its confirmation.
- **Validated inputs.** Everything a caller can put in a URL path or in a
  Proxmox property string (node, storage, snapshot, hostname, bridge, volume
  IDs, task IDs) is checked against a strict character set. This blocks path
  traversal (`..`, `%2F`, `?`, `#`) and option injection (a `,` in `bridge` or
  `storage` cannot add extra options to `net0` or `rootfs`).
- **Downloads.** `download_iso` accepts `https://` URLs only, requires a hex
  checksum and a `.iso` file name, and asks Proxmox to verify certificates.
  Note that Proxmox itself fetches the URL, so it can reach hosts on your
  network: only allow downloads you trust.
- **Token handling.** The token is only ever sent as a header marked sensitive,
  `PROXMOX_URL` must be `https://`, redirects are not followed (so the token
  cannot be forwarded elsewhere), and requests time out after 30 seconds.
  Error messages never include the token.
- **Unprivileged containers** by default. Leave them that way.
- `PROXMOX_INSECURE=1` disables certificate verification, which also lets
  someone on the path read the token. That is acceptable on a trusted local
  network and should be avoided otherwise: install a real certificate on the
  host instead.
- **The model is not a security boundary.** A prompt injected through any text
  Claude reads can ask for any tool you enabled. Keep the write token out of
  sessions that read untrusted content, and review the actions your client asks
  you to approve.

## 8. Rebuild after an incident (CVE, RCE)

A template is only as safe as its last update: no image is flawless. Rebuild
the golden template on a schedule and after every relevant advisory.

1. Build the golden template once: create the container, `apt upgrade`,
   harden it (SSH keys only, no extra services, minimal packages), stop it,
   `convert_to_template`.
2. Keep the data outside the root disk (a separate mount point or volume), so
   rebuilding the system does not mean losing the data.
3. On an incident:
   1. `backup_guest` the compromised guest and keep that backup for analysis.
   2. `guest_power` `stop`, then `delete_guest`.
   3. `clone_guest` from the golden template, `guest_power` `start`.
   4. Restore the data only, never the compromised system. Rotate every
      secret that lived on the guest.

## 9. Troubleshooting

| Symptom | Likely cause |
|---|---|
| `PROXMOX_URL is not set` | variable missing in the MCP client config |
| `PROXMOX_URL must start with https://` | the URL uses `http://` or has no scheme |
| `write operations are disabled` | `PROXMOX_ALLOW_WRITE=1` is missing, by design |
| `401` | token mistyped or expired |
| `403` | the token lacks the required role on `/` (see step 3) |
| TLS certificate error | add `PROXMOX_INSECURE=1` for a self-signed certificate |
| `Task still running after Ns` | long operation, follow it with `get_task_status` |
| `Task completed with warnings` | Proxmox ended the task with `WARNINGS: N`: not a failure, read the message (Debian 13: use `nesting: true`) |
| `invalid identifier`, `invalid volume ID`, `invalid task ID` | a name or ID contains a forbidden character |
| `only backups, ISO images and container templates can be deleted here` | use `delete_guest` for guest disks |
